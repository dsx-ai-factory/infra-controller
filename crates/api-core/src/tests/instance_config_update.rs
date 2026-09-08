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

use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use common::api_fixtures::instance::{
    TestInstance, default_os_config, default_tenant_config, single_interface_network_config,
};
use common::api_fixtures::tenant::create_fixture_tenant;
use common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host,
    create_managed_host_with_config, create_test_env, create_test_env_with_host_inband,
    create_test_env_with_overrides,
};
use config_version::ConfigVersion;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::instance_interface_config::NetworkDetails;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::Request;

use crate::cfg::file::{FnnConfig, FnnRoutingProfileConfig, PrefixFilterPolicyEntry};
use crate::test_support::fixture_config::ManagedHostConfigExt as _;
use crate::test_support::metadata;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::instance::advance_created_instance_into_ready_state;
use crate::tests::common::api_fixtures::{create_managed_host_multi_dpu, get_vpc_fixture_id};
use crate::tests::common::rpc_builder::{
    InstanceAllocationRequest, InstanceConfigExt as _, InstanceConfigUpdateRequest,
    VpcCreationRequest,
};
use crate::tests::common::{self};

/// Returns tenant config matching the shared VPC fixture so update tests reach
/// prefix behavior rather than fail ownership validation.
fn fixture_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
        ..default_tenant_config()
    }
}

/// Compares an expected instance configuration with the actual instance configuration
///
/// We can't directly call `assert_eq` since carbide will fill in details into various fields
/// that are not expected
fn assert_config_equals(
    actual: &rpc::forge::InstanceConfig,
    expected: &rpc::forge::InstanceConfig,
) {
    let mut expected = expected.clone();
    let mut actual = actual.clone();
    if let Some(network) = &mut expected.network {
        network.interfaces.iter_mut().for_each(|x| {
            if let Some(NetworkDetails::VpcPrefixId(_)) = x.network_details {
                x.network_segment_id = None;
            }
        });
    }
    if let Some(network) = &mut actual.network {
        network.interfaces.iter_mut().for_each(|x| {
            if let Some(NetworkDetails::VpcPrefixId(_)) = x.network_details {
                x.network_segment_id = None;
            }
        });
    }
    assert_eq!(expected, actual);
}

/// Compares instance metadata for equality
///
/// Since metadata is transmitted as an unordered list, using `assert_eq!` won't
/// provide expected results
fn assert_metadata_equals(actual: &rpc::forge::Metadata, expected: &rpc::forge::Metadata) {
    let mut actual = actual.clone();
    let mut expected = expected.clone();
    actual.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
    expected.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
    assert_eq!(actual, expected);
}

#[crate::sqlx_test]
async fn test_update_instance_config(_: PgPoolOptions, options: PgConnectOptions) {
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

    let initial_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(initial_os.clone()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: Some("baseline".to_string()),
    };

    let initial_metadata = rpc::Metadata {
        name: "Name1".to_string(),
        description: "Desc1".to_string(),
        labels: vec![],
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(initial_config.clone())
        .metadata(initial_metadata.clone())
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().configs_synced(),
        rpc::forge::SyncState::Synced
    );

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    assert_config_equals(instance.config().inner(), &initial_config);
    assert_metadata_equals(instance.metadata(), &initial_metadata);
    let initial_config_version = instance.config_version();
    assert_eq!(initial_config_version.version_nr(), 1);

    let updated_os_1 = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: true,
        run_provisioning_instructions_on_every_boot: true,
        user_data: Some("SomeRandomData2".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe2".to_string(),
            },
        )),
    };
    let mut updated_config_1 = initial_config.clone();
    updated_config_1.os = Some(updated_os_1);
    updated_config_1.power_profile = Some("balanced".to_string());
    updated_config_1.tenant.as_mut().unwrap().tenant_keyset_ids =
        vec!["a".to_string(), "b".to_string()];
    let updated_metadata_1 = rpc::Metadata {
        name: "Name2".to_string(),
        description: "Desc2".to_string(),
        labels: vec![rpc::forge::Label {
            key: "Key1".to_string(),
            value: None,
        }],
    };

    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(updated_config_1.clone()),
                metadata: Some(updated_metadata_1.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_config_equals(instance.config.as_ref().unwrap(), &updated_config_1);
    assert_metadata_equals(instance.metadata.as_ref().unwrap(), &updated_metadata_1);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 2);

    assert_eq!(
        instance.status.as_ref().unwrap().configs_synced(),
        rpc::forge::SyncState::Pending
    );

    assert_eq!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state(),
        rpc::forge::TenantState::Provisioning
    );

    // Phone home to transition from provisioning to configuring state
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

    // Find our instance details again, which should now
    // be updated.
    let instance = tinstance.rpc_instance().await;

    // Post-phone-home, sync should still be pending, but state Configuring.
    assert_eq!(
        instance.status().configs_synced(),
        rpc::forge::SyncState::Pending
    );

    // And we should be ready from the tenant's perspective.
    assert_eq!(
        instance.status().tenant(),
        rpc::forge::TenantState::Configuring
    );

    // Update the network
    mh.network_configured(&env).await;

    // Find our instance details again, which should now
    // be updated.
    let instance = tinstance.rpc_instance().await;

    // Post-configure, we should now be synced.
    assert_eq!(
        instance.status().configs_synced(),
        rpc::forge::SyncState::Synced
    );

    // And we should be ready from the tenant's perspective.
    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let updated_os_2 = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData3".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe3".to_string(),
            },
        )),
    };
    let mut updated_config_2 = initial_config.clone();
    updated_config_2.os = Some(updated_os_2);
    updated_config_2.power_profile = None;
    updated_config_2.tenant.as_mut().unwrap().tenant_keyset_ids = vec!["c".to_string()];
    let updated_metadata_2 = rpc::Metadata {
        name: "Name12".to_string(),
        description: "".to_string(),
        labels: vec![
            rpc::forge::Label {
                key: "Key11".to_string(),
                value: Some("Value11".to_string()),
            },
            rpc::forge::Label {
                key: "Key12".to_string(),
                value: None,
            },
        ],
    };

    // Start a conditional update first that specifies the wrong last version.
    // This should fail.
    let status = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: Some(initial_config_version.version_string()),
                config: Some(updated_config_2.clone()),
                metadata: Some(updated_metadata_2.clone()),
            },
        ))
        .await
        .expect_err("RPC call should fail with PreconditionFailed error");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        status.message(),
        format!(
            "an object of type instance was intended to be modified did not have the expected version {}",
            initial_config_version.version_string()
        ),
        "Message is {}",
        status.message()
    );

    // Using the correct current version should allow the update
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: Some(updated_config_version.version_string()),
                config: Some(updated_config_2.clone()),
                metadata: Some(updated_metadata_2.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let mut expected_config_2 = updated_config_2.clone();
    expected_config_2.power_profile = Some("balanced".to_string());
    assert_config_equals(instance.config.as_ref().unwrap(), &expected_config_2);
    assert_metadata_equals(instance.metadata.as_ref().unwrap(), &updated_metadata_2);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 3);

    let mut clear_power_profile_config = expected_config_2.clone();
    clear_power_profile_config.power_profile = Some(String::new());
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: Some(updated_config_version.version_string()),
                config: Some(clear_power_profile_config),
                metadata: Some(updated_metadata_2.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(instance.config.unwrap().power_profile, None);
    assert_eq!(
        instance
            .config_version
            .parse::<ConfigVersion>()
            .unwrap()
            .version_nr(),
        4
    );

    // Try to update a non-existing instance
    let unknown_instance = uuid::Uuid::new_v4();
    let status = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(unknown_instance.into()),
                if_version_match: None,
                config: Some(updated_config_2.clone()),
                metadata: Some(updated_metadata_2.clone()),
            },
        ))
        .await
        .expect_err("RPC call should fail with NotFound error");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(
        status.message(),
        format!("instance not found: {unknown_instance}"),
        "Message is {}",
        status.message()
    );
}

#[crate::sqlx_test]
async fn test_update_instance_config_restores_deprecated_auto_config(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_test_support::Outcome::FailsWith;
    use carbide_test_support::{Case, check_cases_async};

    let env = create_test_env_with_host_inband(pool).await;
    let (flat_vpc_id, _) =
        common::api_fixtures::vpc::create_flat_vpc(&env, "legacy-auto-update".to_string(), None)
            .await;
    let (different_flat_vpc_id, _) =
        common::api_fixtures::vpc::create_flat_vpc(&env, "different-auto-update".to_string(), None)
            .await;

    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let managed_host = create_managed_host_with_config(&env, ManagedHostConfig::zero_dpu()).await;
    let mut txn = env.db_txn().await;
    let host_inband_segment =
        db::network_segment::find_by_name(txn.as_mut(), "HOST_INBAND").await?;
    assert!(
        host_inband_segment.config.vpc_id.is_none(),
        "the compatibility path must not depend on a segment VPC binding"
    );
    drop(txn);

    let initial_metadata = rpc::Metadata {
        name: "legacy-auto-update".to_string(),
        description: "initial metadata".to_string(),
        labels: vec![],
    };
    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(rpc::InstanceConfig::default_tenant_and_os().network(
                    rpc::InstanceNetworkConfig {
                        interfaces: vec![],
                        #[allow(deprecated)]
                        auto: true,
                        auto_config: Some(rpc::forge::InstanceNetworkAutoConfig {
                            vpc_id: Some(flat_vpc_id),
                        }),
                    },
                ))
                .metadata(initial_metadata)
                .tonic_request(),
        )
        .await?
        .into_inner();

    let mut legacy_config = instance
        .config
        .clone()
        .expect("instance config must be set");
    let legacy_network = legacy_config
        .network
        .as_mut()
        .expect("instance network config must be set");
    #[allow(deprecated)]
    {
        legacy_network.auto = true;
    }
    legacy_network.auto_config = None;

    let mut updated_metadata = instance
        .metadata
        .clone()
        .expect("instance metadata must be set");
    updated_metadata.description = "updated through the legacy wire format".to_string();
    let updated = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: instance.id,
            if_version_match: None,
            config: Some(legacy_config),
            metadata: Some(updated_metadata.clone()),
        }))
        .await?
        .into_inner();

    assert_eq!(updated.metadata.as_ref(), Some(&updated_metadata));
    let updated_network = updated
        .config
        .as_ref()
        .and_then(|config| config.network.as_ref())
        .expect("updated instance network config must be set");
    #[allow(deprecated)]
    let updated_auto = updated_network.auto;
    assert!(updated_auto, "automatic networking must remain enabled");
    assert_eq!(
        updated_network
            .auto_config
            .as_ref()
            .and_then(|config| config.vpc_id),
        Some(flat_vpc_id),
        "the stored VPC must survive a legacy update"
    );
    assert!(
        updated_network.interfaces.is_empty(),
        "resolved HostInband interfaces must remain internal"
    );

    let mut disable_auto = updated_network.clone();
    #[allow(deprecated)]
    {
        disable_auto.auto = false;
    }
    disable_auto.auto_config = None;

    let mut explicit_interface = updated_network.clone();
    explicit_interface.auto_config = None;
    explicit_interface.interfaces = vec![rpc::InstanceInterfaceConfig {
        function_type: rpc::InterfaceFunctionType::Physical as i32,
        network_segment_id: Some(host_inband_segment.id),
        network_details: None,
        device: None,
        device_instance: 0,
        virtual_function_id: None,
        ip_address: None,
        ipv6_interface_config: None,
        routing_profile: None,
    }];

    let mut different_auto_config = updated_network.clone();
    different_auto_config.auto_config = Some(rpc::forge::InstanceNetworkAutoConfig {
        vpc_id: Some(different_flat_vpc_id),
    });

    let mut incomplete_auto_config = updated_network.clone();
    incomplete_auto_config.auto_config =
        Some(rpc::forge::InstanceNetworkAutoConfig { vpc_id: None });

    struct RejectInput {
        network: rpc::InstanceNetworkConfig,
        expected_message: &'static str,
    }

    check_cases_async(
        [
            Case {
                scenario: "deprecated auto=false cannot disable automatic networking",
                input: RejectInput {
                    network: disable_auto,
                    expected_message: "cannot change `InstanceNetworkConfig.auto_config`",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "deprecated auto with explicit interfaces remains invalid",
                input: RejectInput {
                    network: explicit_interface,
                    expected_message: "cannot change `InstanceNetworkConfig.auto_config`",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "an explicit different auto_config remains authoritative",
                input: RejectInput {
                    network: different_auto_config,
                    expected_message: "cannot change `InstanceNetworkConfig.auto_config`",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "a present but incomplete auto_config remains a conversion error",
                input: RejectInput {
                    network: incomplete_auto_config,
                    expected_message: "vpc_id",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
        ],
        |RejectInput {
             network,
             expected_message,
         }| {
            let env = &env;
            let updated = &updated;
            async move {
                let mut rejected_config =
                    updated.config.clone().expect("instance config must be set");
                rejected_config.network = Some(network);
                env.api
                    .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                        instance_id: updated.id,
                        if_version_match: None,
                        config: Some(rejected_config),
                        metadata: updated.metadata.clone(),
                    }))
                    .await
                    .map(|_| ())
                    .map_err(|err| (err.code(), err.message().contains(expected_message)))
            }
        },
    )
    .await;

    Ok(())
}

#[crate::sqlx_test]
async fn test_reject_invalid_instance_config_updates(_: PgPoolOptions, options: PgConnectOptions) {
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

    let valid_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(initial_os.clone()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let initial_metadata = rpc::Metadata {
        name: "Name1".to_string(),
        description: "Desc1".to_string(),
        labels: vec![],
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(valid_config.clone())
        .metadata(initial_metadata.clone())
        .build()
        .await;

    // Try to update to an invalid OS
    let invalid_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: true,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData2".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "".to_string(),
            },
        )),
    };
    let mut invalid_os_config = valid_config.clone();
    invalid_os_config.os = Some(invalid_os);
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(invalid_os_config),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("Invalid OS should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "invalid value: InlineIpxe::ipxe_script is empty"
    );

    // The tenant of an instance can not be updated
    let mut config_with_updated_tenant = valid_config.clone();
    config_with_updated_tenant
        .tenant
        .as_mut()
        .unwrap()
        .tenant_organization_id = "new_tenant".to_string();
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(config_with_updated_tenant),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("New tenant should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "configuration value cannot be modified: TenantConfig::tenant_organization_id"
    );

    // A deprecated auto request cannot turn an explicitly networked instance into an auto one.
    let mut deprecated_auto_config = valid_config.clone();
    let deprecated_auto_network = deprecated_auto_config
        .network
        .as_mut()
        .expect("network config must be set");
    deprecated_auto_network.interfaces.clear();
    #[allow(deprecated)]
    {
        deprecated_auto_network.auto = true;
    }
    deprecated_auto_network.auto_config = None;
    let err = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            if_version_match: None,
            config: Some(deprecated_auto_config),
            metadata: Some(initial_metadata.clone()),
        }))
        .await
        .expect_err("deprecated auto must not enable automatic networking");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("deprecated `InstanceNetworkConfig.auto`"),
        "unexpected error: {err}"
    );

    // Requesting IPs is not allowed with network segments.
    let mut config_with_bad_updated_interfaces = valid_config.clone();
    config_with_bad_updated_interfaces
        .network
        .as_mut()
        .unwrap()
        .interfaces = vec![rpc::forge::InstanceInterfaceConfig {
        function_type: rpc::forge::InterfaceFunctionType::Physical as _,
        network_segment_id: Some(NetworkSegmentId::new()),
        network_details: None,
        device: None,
        device_instance: 0u32,
        virtual_function_id: None,
        ip_address: Some("192.168.0.1".to_string()),
        ipv6_interface_config: None,
        routing_profile: None,
    }];

    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(config_with_bad_updated_interfaces),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("IP request with network segment should not be allowed");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("explicit IP requests are only supported for VPC prefixes")
    );

    // The network configuration of an instance can not be updated
    let mut config_with_updated_network = valid_config.clone();
    config_with_updated_network
        .network
        .as_mut()
        .unwrap()
        .interfaces
        .clear();

    // instance network config update is allowed now.
    config_with_updated_network
        .network
        .as_mut()
        .unwrap()
        .interfaces
        .push(rpc::forge::InstanceInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Virtual as _,
            network_segment_id: Some(NetworkSegmentId::new()),
            network_details: None,
            device: None,
            device_instance: 0u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        });
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(config_with_updated_network),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("New network configuration should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .starts_with("invalid value: Missing Physical Function")
    );

    // Try to update to duplicated tenant keyset IDs
    let mut duplicated_keysets_config = valid_config.clone();
    duplicated_keysets_config
        .tenant
        .as_mut()
        .unwrap()
        .tenant_keyset_ids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(duplicated_keysets_config),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("Duplicate keyset IDs should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "duplicate tenant KeySet ID found: a");

    // Try to update to over max tenant keyset IDs
    let mut maxed_keysets_config = valid_config.clone();
    maxed_keysets_config
        .tenant
        .as_mut()
        .unwrap()
        .tenant_keyset_ids = vec![
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
    ];
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(maxed_keysets_config),
                metadata: Some(initial_metadata.clone()),
            },
        ))
        .await
        .expect_err("Over max keyset config should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "more than 10 tenant KeySet IDs are not allowed"
    );

    // Try to update to invalid metadata
    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(true) {
        let err = env
            .api
            .update_instance_config(tonic::Request::new(
                rpc::forge::InstanceConfigUpdateRequest {
                    instance_id: Some(tinstance.id),
                    if_version_match: None,
                    config: Some(valid_config.clone()),
                    metadata: Some(invalid_metadata.clone()),
                },
            ))
            .await
            .expect_err(&format!(
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
async fn test_update_instance_config_rejects_interface_anycast_prefix_outside_vpc_profile(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let profile_type = "ANYCAST_UPDATE_TEST";
    let tenant_org = "anycast-update-test";

    // Configure the operator-owned VPC profile with one allowed anycast prefix.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default().with_fnn_config(Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([(
                profile_type.to_string(),
                FnnRoutingProfileConfig {
                    internal: Some(true),
                    access_tier: Some(0),
                    allowed_anycast_prefixes: Some(vec![PrefixFilterPolicyEntry {
                        prefix: "192.0.2.0/24".parse().unwrap(),
                    }]),
                    ..Default::default()
                },
            )]),
            use_vpc_vrf_loopback: false,
        })),
    )
    .await;

    // Create a tenant and FNN VPC that use that routing profile.
    env.api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: tenant_org.to_string(),
            routing_profile_type: Some(profile_type.to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: tenant_org.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant_org)
                .metadata(rpc::forge::Metadata {
                    name: "anycast update vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type(profile_type.to_string())
                .rpc(),
        )
        .await;

    // Allocate a ready instance before requesting the invalid routing-profile update.
    let mh = create_managed_host(&env).await;
    let tinstance = mh
        .instance_builer(&env)
        .tenant_org(tenant_org)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let instance = tinstance.rpc_instance().await;

    // Request an interface anycast prefix outside the owning VPC profile.
    let mut network_config = single_interface_network_config(segment_id);
    network_config.interfaces[0].routing_profile =
        Some(rpc::forge::InstanceInterfaceRoutingProfile {
            allowed_anycast_prefixes: vec![rpc::forge::PrefixFilterPolicyEntry {
                prefix: "198.51.100.0/24".to_string(),
            }],
        });

    // Update the instance and verify invalid tenant input is rejected before queuing work.
    let err = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(rpc::TenantConfig {
                        tenant_organization_id: tenant_org.to_string(),
                        tenant_keyset_ids: vec![],
                        hostname: None,
                    }),
                    os: Some(common::api_fixtures::instance::default_os_config()),
                    network: Some(network_config),
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
        .expect_err("interface anycast prefix outside VPC profile should be rejected");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("routing_profile.allowed_anycast_prefixes")
    );
}

#[crate::sqlx_test]
async fn test_update_instance_config_vpc_prefix_no_network_update(
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
    let ip_prefix = "192.1.4.0/25";
    let vpc_id = get_vpc_fixture_id(&env).await;
    let new_vpc_prefix = rpc::forge::VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let response = env
        .api
        .create_vpc_prefix(request)
        .await
        .unwrap()
        .into_inner();

    let mut network = single_interface_network_config(segment_id);
    network.interfaces.iter_mut().for_each(|x| {
        x.network_segment_id = None;
        x.network_details = response.id.map(NetworkDetails::VpcPrefixId);
    });
    let initial_config = rpc::InstanceConfig {
        tenant: Some(fixture_tenant_config()),
        os: Some(initial_os.clone()),
        network: Some(network.clone()),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let initial_metadata = rpc::Metadata {
        name: "Name1".to_string(),
        description: "Desc1".to_string(),
        labels: vec![],
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(initial_config.clone())
        .metadata(initial_metadata.clone())
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().configs_synced(),
        rpc::forge::SyncState::Synced
    );

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    assert_config_equals(instance.config().inner(), &initial_config);
    assert_metadata_equals(instance.metadata(), &initial_metadata);
    let initial_config_version = instance.config_version();
    assert_eq!(initial_config_version.version_nr(), 1);

    let mut updated_config_1 = initial_config.clone();
    updated_config_1.network = Some(network);
    let updated_metadata_1 = rpc::Metadata {
        name: "Name2".to_string(),
        description: "Desc2".to_string(),
        labels: vec![rpc::forge::Label {
            key: "Key1".to_string(),
            value: None,
        }],
    };

    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(updated_config_1.clone()),
                metadata: Some(updated_metadata_1.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_config_equals(instance.config.as_ref().unwrap(), &updated_config_1);
    assert_metadata_equals(instance.metadata.as_ref().unwrap(), &updated_metadata_1);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 2);

    assert_eq!(
        instance.status.as_ref().unwrap().configs_synced(),
        rpc::forge::SyncState::Pending
    );

    // SyncState::Synced means network config update is not applicable.
    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::forge::SyncState::Synced
    );
}

/// Pairs an eligible FNN VPC with its prefix so selector intent can be compared
/// with the expected resolved allocation.
#[derive(Clone, Copy)]
struct VpcPrefixFixture {
    vpc_id: VpcId,
    vpc_prefix_id: VpcPrefixId,
}

/// Active resources expected to survive replacing an explicitly selected
/// prefix with equivalent automatic VPC intent.
struct ActiveVpcResources {
    network_segment_id: NetworkSegmentId,
    addresses: Vec<String>,
    internal_interface: model::instance::config::network::InstanceInterfaceConfig,
}

/// Creates an FNN VPC with the requested prefix capacity so update scenarios
/// reach selector behavior rather than fail its eligibility check.
async fn create_fnn_vpc_prefix_fixture(
    env: &TestEnv,
    tenant_organization_id: &str,
    vpc_name: &str,
    vpc_prefix_name: &str,
    prefix: &str,
    slaac_enabled: bool,
) -> VpcPrefixFixture {
    // Automatic selection accepts only FNN VPCs.
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(rpc::Metadata {
                    name: vpc_name.to_string(),
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

    let prefix = prefix.parse::<ipnetwork::IpNetwork>().unwrap();
    let vpc_prefix_id = if prefix.is_ipv6() {
        // Persist IPv6 prefixes directly so tests of the update policy are
        // independent of the fixture for Site fabric prefixes and its IPv4-only
        // containment policy.
        let mut txn = env.db_txn().await;
        let vpc = db::vpc::find_by(
            txn.as_mut(),
            db::ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
        let vpc_prefix_id = db::vpc_prefix::persist(
            model::vpc_prefix::NewVpcPrefix {
                id: uuid::Uuid::new_v4().into(),
                site_prefix_id: None,
                vpc_id,
                config: model::vpc_prefix::VpcPrefixConfig { prefix },
                metadata: model::metadata::Metadata {
                    name: vpc_prefix_name.to_string(),
                    ..Default::default()
                },
            },
            vpc.version,
            &mut txn,
        )
        .await
        .unwrap()
        .id;
        txn.commit().await.unwrap();
        vpc_prefix_id
    } else {
        env.api
            .create_vpc_prefix(Request::new(rpc::forge::VpcPrefixCreationRequest {
                id: None,
                prefix: String::new(),
                vpc_id: Some(vpc_id),
                site_prefix_id: None,
                config: Some(rpc::forge::VpcPrefixConfig {
                    prefix: prefix.to_string(),
                }),
                metadata: Some(rpc::Metadata {
                    name: vpc_prefix_name.to_string(),
                    ..Default::default()
                }),
            }))
            .await
            .unwrap()
            .into_inner()
            .id
            .unwrap()
    };

    VpcPrefixFixture {
        vpc_id,
        vpc_prefix_id,
    }
}

/// Builds one physical interface so update scenarios can vary only the caller's
/// VPC or prefix intent.
fn single_vpc_interface_network(network_details: NetworkDetails) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: Some(network_details),
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

/// Captures the explicit allocation baseline so later stages can prove an
/// intent-only update does not churn its segment or addresses.
async fn observe_active_vpc_resources(
    env: &TestEnv,
    tinstance: &TestInstance<'_, '_>,
    fixture: VpcPrefixFixture,
) -> ActiveVpcResources {
    // Verify the public projection exposes explicit intent and its resolved prefix.
    let initial = tinstance.rpc_instance().await;
    let initial_interface = &initial.config().network().interfaces[0];
    let network_segment_id = initial_interface.network_segment_id.unwrap();
    assert_eq!(
        initial_interface.network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.vpc_prefix_id)),
    );
    let initial_status_interface = &initial.status().network().interfaces[0];
    assert_eq!(
        initial_status_interface
            .resolved_vpc_prefixes
            .as_ref()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );
    let addresses = initial_status_interface.addresses.clone();
    assert!(!addresses.is_empty());

    // Preserve internal allocation state that is not fully exposed through RPC.
    let mut txn = env.pool.begin().await.unwrap();
    let initial_snapshot = tinstance.db_instance(&mut txn).await;
    let internal_interface = initial_snapshot.config.network.interfaces[0].clone();
    txn.rollback().await.unwrap();

    ActiveVpcResources {
        network_segment_id,
        addresses,
        internal_interface,
    }
}

/// Stages automatic intent while retaining the explicit RPC projection because
/// pending intent must not become publicly active before controller promotion.
async fn stage_automatic_vpc_update(
    env: &TestEnv,
    tinstance: &TestInstance<'_, '_>,
    config: &rpc::InstanceConfig,
    metadata: &rpc::Metadata,
    fixture: VpcPrefixFixture,
    active: &ActiveVpcResources,
) {
    // Submit the complete replacement configuration with automatic intent.
    let response = env
        .api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(tinstance.id)
                .config(config.clone())
                .metadata(metadata.clone())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Until controller promotion, the response continues exposing active resources.
    let response_interface = &response
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert_eq!(
        response_interface.network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.vpc_prefix_id)),
    );
    assert_eq!(
        response_interface.network_segment_id,
        Some(active.network_segment_id),
    );
    let response_status_interface = &response
        .status
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert_eq!(
        response_status_interface
            .resolved_vpc_prefixes
            .as_ref()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );
}

/// Verifies a pending re-read retains active allocation identity but withholds
/// addresses because staged networking remains unsynchronized.
async fn assert_pending_inventory_reuses_active_resources(
    tinstance: &TestInstance<'_, '_>,
    fixture: VpcPrefixFixture,
    active: &ActiveVpcResources,
) {
    // Re-read before controller promotion, while automatic intent remains staged.
    let pending = tinstance.rpc_instance().await;
    let pending_interface = &pending.config().network().interfaces[0];
    assert_eq!(
        pending_interface.network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.vpc_prefix_id)),
    );
    assert_eq!(
        pending_interface.network_segment_id,
        Some(active.network_segment_id),
    );
    assert_eq!(
        pending.status().network().interfaces[0]
            .resolved_vpc_prefixes
            .as_ref()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );

    // Pending status reports unsynchronized networking and withholds addresses.
    let pending_status = pending.status().network();
    assert_eq!(pending_status.configs_synced(), rpc::SyncState::Pending);
    assert!(pending_status.interfaces[0].addresses.is_empty());
}

/// Verifies the staged request carries automatic intent while reusing active
/// allocations because the explicit prefix already satisfies the same VPC selector.
async fn assert_staged_automatic_vpc_request(
    env: &TestEnv,
    tinstance: &TestInstance<'_, '_>,
    fixture: VpcPrefixFixture,
    active: &ActiveVpcResources,
) {
    // Pending automatic intent is hidden from RPC until promotion, so inspect it internally.
    let mut txn = env.pool.begin().await.unwrap();
    let pending_snapshot = tinstance.db_instance(&mut txn).await;
    let pending_request = pending_snapshot
        .update_network_config_request
        .as_ref()
        .unwrap();

    // The staged selector must retain every active network allocation.
    let staged_interface = &pending_request.new_config.interfaces[0];
    let staged_selection = staged_interface.vpc_selection.as_ref().unwrap();
    assert_eq!(staged_selection.vpc_id, fixture.vpc_id);
    assert_eq!(
        staged_selection.family_mode,
        model::instance::config::network::InstanceInterfaceIpFamilyMode::Ipv4Only,
    );
    assert_eq!(
        staged_interface.generated_network_segment_id(),
        Some(active.network_segment_id),
    );
    assert_eq!(
        staged_interface
            .resolved_vpc_prefixes()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );
    assert_eq!(
        staged_interface.ip_addrs,
        active.internal_interface.ip_addrs,
    );
    txn.rollback().await.unwrap();
}

/// Promotes the selector without allocation churn because the active explicit
/// prefix already satisfies the automatic VPC intent.
async fn promote_automatic_vpc_request(
    env: &TestEnv,
    mh: &TestManagedHost,
    tinstance: &TestInstance<'_, '_>,
    fixture: VpcPrefixFixture,
    active: &ActiveVpcResources,
) -> ConfigVersion {
    // The generated segment is reused, so no network segment controller iteration is required.
    env.run_machine_state_controller_iteration_network_config_return_to_ready(mh, false)
        .await;

    // After DPU synchronization returns the instance to Ready, RPC exposes the selector.
    let promoted = tinstance.rpc_instance().await;
    let promoted_network_version = promoted.network_config_version();
    let promoted_interface = &promoted.config().network().interfaces[0];
    let promoted_selection = match promoted_interface.network_details.as_ref() {
        Some(NetworkDetails::Vpc(selection)) => selection,
        other => panic!("expected automatic VPC intent after promotion, got {other:?}"),
    };
    assert_eq!(promoted_selection.vpc_id, Some(fixture.vpc_id));
    assert_eq!(
        promoted_selection.family_mode,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv4Only as i32,
    );
    assert_eq!(
        promoted_interface.network_segment_id,
        Some(active.network_segment_id),
    );
    let promoted_status_interface = &promoted.status().network().interfaces[0];
    assert_eq!(promoted_status_interface.addresses, active.addresses);
    assert_eq!(
        promoted_status_interface
            .resolved_vpc_prefixes
            .as_ref()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );

    promoted_network_version
}

/// Resubmits the selector without changing version or segment because the
/// complete network configuration is already active.
async fn repeat_automatic_vpc_update(
    env: &TestEnv,
    tinstance: &TestInstance<'_, '_>,
    config: &rpc::InstanceConfig,
    metadata: &rpc::Metadata,
    active: &ActiveVpcResources,
    promoted_network_version: &ConfigVersion,
) {
    // Submit the same complete configuration after promotion.
    let repeated = env
        .api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(tinstance.id)
                .config(config.clone())
                .metadata(metadata.clone())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // A network no-op preserves the network version and generated segment.
    assert_eq!(
        repeated.network_config_version,
        promoted_network_version.to_string(),
    );
    let repeated_interface = &repeated
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert_eq!(
        repeated_interface.network_segment_id,
        Some(active.network_segment_id),
    );
}

/// Confirms replay creates no staged work or allocation churn because the
/// identical selector and resolved resources are already active.
async fn assert_repeated_update_reuses_active_resources(
    env: &TestEnv,
    tinstance: &TestInstance<'_, '_>,
    fixture: VpcPrefixFixture,
    active: &ActiveVpcResources,
    promoted_network_version: &ConfigVersion,
) {
    // An identical update must leave no controller work or allocation changes.
    let mut txn = env.pool.begin().await.unwrap();
    let repeated_snapshot = tinstance.db_instance(&mut txn).await;
    assert!(repeated_snapshot.update_network_config_request.is_none());
    assert_eq!(
        &repeated_snapshot.network_config_version,
        promoted_network_version,
    );
    let repeated_interface = &repeated_snapshot.config.network.interfaces[0];
    assert_eq!(
        repeated_interface.generated_network_segment_id(),
        Some(active.network_segment_id),
    );
    assert_eq!(
        repeated_interface
            .resolved_vpc_prefixes()
            .unwrap()
            .ipv4_vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );
    assert_eq!(
        repeated_interface.ip_addrs,
        active.internal_interface.ip_addrs,
    );

    // The reused segment must remain active and bound to the original prefix.
    let reused_segments = db::network_segment::find_by(
        txn.as_mut(),
        db::ObjectColumnFilter::One(db::network_segment::IdColumn, &active.network_segment_id),
        Default::default(),
    )
    .await
    .unwrap();
    let [reused_segment] = reused_segments.as_slice() else {
        panic!("expected the reused generated network segment to remain present");
    };
    assert!(!reused_segment.is_marked_as_deleted());
    assert_eq!(reused_segment.prefixes.len(), 1);
    assert_eq!(
        reused_segment.prefixes[0].vpc_prefix_id,
        Some(fixture.vpc_prefix_id),
    );
    txn.rollback().await.unwrap();
}

/// Verifies equivalent explicit-to-automatic intent promotes without
/// reallocation, then confirms replay remains a network no-op.
#[crate::sqlx_test]
async fn test_update_explicit_vpc_prefix_to_automatic_vpc_reuses_active_resources(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Create one eligible FNN VPC and allocate a ready instance from its explicit prefix.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let tenant = default_tenant_config();
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    create_fixture_tenant(&env, tenant.tenant_organization_id.clone())
        .await
        .unwrap();
    let fixture = create_fnn_vpc_prefix_fixture(
        &env,
        tenant.tenant_organization_id.as_str(),
        "explicit-to-automatic-vpc",
        "explicit-to-automatic-prefix",
        "192.1.4.0/25",
        false,
    )
    .await;
    let mh = create_managed_host(&env).await;
    let metadata = rpc::Metadata {
        name: "explicit-to-automatic-instance".to_string(),
        description: "tests/instance_config_update".to_string(),
        labels: Vec::new(),
    };
    let initial_network =
        single_vpc_interface_network(NetworkDetails::VpcPrefixId(fixture.vpc_prefix_id));
    let initial_config = rpc::InstanceConfig {
        tenant: Some(tenant),
        os: Some(default_os_config()),
        network: Some(initial_network),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    let tinstance = mh
        .instance_builer(&env)
        .config(initial_config.clone())
        .metadata(metadata.clone())
        .build()
        .await;

    // Capture the active segment and addresses that the selector transition must reuse.
    let active = observe_active_vpc_resources(&env, &tinstance, fixture).await;

    // Change only caller intent to automatic selection of the same VPC.
    let automatic_network = single_vpc_interface_network(NetworkDetails::Vpc(
        rpc::forge::InstanceInterfaceVpcSelection {
            vpc_id: Some(fixture.vpc_id),
            family_mode: rpc::forge::InstanceInterfaceIpFamilyMode::Ipv4Only as i32,
        },
    ));
    let mut automatic_config = initial_config;
    automatic_config.network = Some(automatic_network);

    // Verify the selector is staged while RPC still exposes the active explicit allocation.
    stage_automatic_vpc_update(
        &env,
        &tinstance,
        &automatic_config,
        &metadata,
        fixture,
        &active,
    )
    .await;
    assert_pending_inventory_reuses_active_resources(&tinstance, fixture, &active).await;
    assert_staged_automatic_vpc_request(&env, &tinstance, fixture, &active).await;

    // Promote the selector, then repeat the complete update to verify network no-op reuse.
    let promoted_network_version =
        promote_automatic_vpc_request(&env, &mh, &tinstance, fixture, &active).await;
    repeat_automatic_vpc_update(
        &env,
        &tinstance,
        &automatic_config,
        &metadata,
        &active,
        &promoted_network_version,
    )
    .await;
    assert_repeated_update_reuses_active_resources(
        &env,
        &tinstance,
        fixture,
        &active,
        &promoted_network_version,
    )
    .await;
}

/// Existing resources must not erase a newly requested address before validating the family of an
/// explicitly selected prefix and the SLAAC policy.
#[crate::sqlx_test]
async fn test_update_slaac_vpc_rejects_explicit_ipv6_before_resource_reuse(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let tenant = default_tenant_config();
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    create_fixture_tenant(&env, tenant.tenant_organization_id.clone())
        .await
        .unwrap();
    let ipv6_fixture = create_fnn_vpc_prefix_fixture(
        &env,
        tenant.tenant_organization_id.as_str(),
        "SLAAC update validation VPC",
        "SLAAC update IPv6 prefix",
        // Three table rows allocate an IPv6 prefix before exercising update
        // validation. A /62 supplies four SLAAC /64 allocations.
        "fd42:2403:1::/62",
        true,
    )
    .await;
    let ipv4_prefix_id = env
        .api
        .create_vpc_prefix(Request::new(rpc::forge::VpcPrefixCreationRequest {
            id: None,
            prefix: String::new(),
            vpc_id: Some(ipv6_fixture.vpc_id),
            site_prefix_id: None,
            config: Some(rpc::forge::VpcPrefixConfig {
                // This one prefix backs every table row below; leave enough /31
                // linknets for each initial instance allocation.
                prefix: "192.1.4.0/28".to_string(),
            }),
            metadata: Some(rpc::Metadata {
                name: "SLAAC update IPv4 prefix".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    enum RequestedIpv6Shape {
        Primary,
        DualStackSidecar,
    }

    struct UpdateCase {
        scenario: &'static str,
        initial_network: rpc::InstanceNetworkConfig,
        requested_address: &'static str,
        shape: RequestedIpv6Shape,
        expected_error_fragments: [&'static str; 2],
    }

    let ipv6_primary =
        single_vpc_interface_network(NetworkDetails::VpcPrefixId(ipv6_fixture.vpc_prefix_id));
    let mut dual_stack = single_vpc_interface_network(NetworkDetails::VpcPrefixId(ipv4_prefix_id));
    dual_stack.interfaces[0].ipv6_interface_config =
        Some(rpc::forge::InstanceInterfaceIpv6Config {
            vpc_prefix_id: Some(ipv6_fixture.vpc_prefix_id),
            ip_address: None,
        });

    // Keep both explicit IPv6 fields exposed to callers and the mismatch with the primary address
    // family in one table so this earlier validation cannot drift from canonical allocation errors.
    for (case_index, case) in [
        UpdateCase {
            scenario: "IPv6-only primary prefix",
            initial_network: ipv6_primary.clone(),
            requested_address: "fd42:2403:1::1",
            shape: RequestedIpv6Shape::Primary,
            expected_error_fragments: ["requested IPv6 address", "has SLAAC enabled"],
        },
        UpdateCase {
            scenario: "dual-stack IPv6 sidecar",
            initial_network: dual_stack,
            requested_address: "fd42:2403:1::3",
            shape: RequestedIpv6Shape::DualStackSidecar,
            expected_error_fragments: ["requested IPv6 address", "has SLAAC enabled"],
        },
        UpdateCase {
            scenario: "IPv6 request against an IPv4 primary prefix",
            initial_network: single_vpc_interface_network(NetworkDetails::VpcPrefixId(
                ipv4_prefix_id,
            )),
            requested_address: "fd42:2403:1::2",
            shape: RequestedIpv6Shape::Primary,
            expected_error_fragments: ["requested IP address", "does not match VPC prefix"],
        },
        UpdateCase {
            scenario: "IPv4 request against an IPv6 primary prefix",
            initial_network: ipv6_primary,
            requested_address: "192.0.2.1",
            shape: RequestedIpv6Shape::Primary,
            expected_error_fragments: ["requested IP address", "does not match VPC prefix"],
        },
    ]
    .into_iter()
    .enumerate()
    {
        let managed_host = create_managed_host(&env).await;
        let metadata = rpc::Metadata {
            name: format!("SLAAC update validation {case_index}"),
            description: case.scenario.to_string(),
            labels: Vec::new(),
        };
        let initial_config = rpc::InstanceConfig {
            tenant: Some(tenant.clone()),
            os: Some(default_os_config()),
            network: Some(case.initial_network),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        };
        let instance = managed_host
            .instance_builer(&env)
            .config(initial_config.clone())
            .metadata(metadata.clone())
            .build()
            .await;

        let mut txn = env.db_txn().await;
        let before = instance.db_instance(&mut txn).await;
        txn.rollback().await.unwrap();
        assert!(before.update_network_config_request.is_none());

        let mut requested_config = initial_config;
        let requested_interface = &mut requested_config.network.as_mut().unwrap().interfaces[0];
        match case.shape {
            RequestedIpv6Shape::Primary => {
                requested_interface.ip_address = Some(case.requested_address.to_string());
            }
            RequestedIpv6Shape::DualStackSidecar => {
                requested_interface
                    .ipv6_interface_config
                    .as_mut()
                    .unwrap()
                    .ip_address = Some(case.requested_address.to_string());
            }
        }

        let error = env
            .api
            .update_instance_config(
                InstanceConfigUpdateRequest::builder()
                    .instance_id(instance.id)
                    .config(requested_config)
                    .metadata(metadata)
                    .tonic_request(),
            )
            .await
            .expect_err(case.scenario);
        assert_eq!(
            error.code(),
            tonic::Code::InvalidArgument,
            "{}",
            case.scenario
        );
        for expected_fragment in case.expected_error_fragments {
            assert!(
                error.message().contains(expected_fragment),
                "unexpected {} error: {error}",
                case.scenario,
            );
        }

        let mut txn = env.db_txn().await;
        let after = instance.db_instance(&mut txn).await;
        txn.rollback().await.unwrap();
        assert_eq!(
            serde_json::to_value(&after.config).unwrap(),
            serde_json::to_value(&before.config).unwrap(),
            "{} changed config",
            case.scenario,
        );
        assert_eq!(
            after.config_version, before.config_version,
            "{} changed config version",
            case.scenario,
        );
        assert_eq!(
            after.network_config_version, before.network_config_version,
            "{} changed network config version",
            case.scenario,
        );
        assert!(
            after.update_network_config_request.is_none(),
            "{} staged a pending network update",
            case.scenario,
        );
    }

    // Ownership validation must precede the SLAAC policy error so an update
    // cannot disclose another tenant's VPC mode.
    let foreign_tenant_organization_id = "slaac-update-foreign-tenant";
    create_fixture_tenant(&env, foreign_tenant_organization_id)
        .await
        .unwrap();
    let foreign_fixture = create_fnn_vpc_prefix_fixture(
        &env,
        foreign_tenant_organization_id,
        "Foreign SLAAC update validation VPC",
        "Foreign SLAAC update IPv6 prefix",
        "fd42:2403:2::/126",
        true,
    )
    .await;
    let managed_host = create_managed_host(&env).await;
    let metadata = rpc::Metadata {
        name: "SLAAC update ownership validation".to_string(),
        ..Default::default()
    };
    let initial_config = rpc::InstanceConfig {
        tenant: Some(tenant.clone()),
        os: Some(default_os_config()),
        network: Some(single_vpc_interface_network(NetworkDetails::VpcPrefixId(
            ipv4_prefix_id,
        ))),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    let instance = managed_host
        .instance_builer(&env)
        .config(initial_config.clone())
        .metadata(metadata.clone())
        .build()
        .await;

    let mut requested_config = initial_config;
    let mut requested_network =
        single_vpc_interface_network(NetworkDetails::VpcPrefixId(foreign_fixture.vpc_prefix_id));
    requested_network.interfaces[0].ip_address = Some("fd42:2403:2::1".to_string());
    requested_config.network = Some(requested_network);
    let error = env
        .api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(instance.id)
                .config(requested_config)
                .metadata(metadata)
                .tonic_request(),
        )
        .await
        .expect_err("foreign VPC prefix must fail ownership validation");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error.message().contains("which is not owned by tenant")
            && !error.message().contains("has SLAAC enabled"),
        "unexpected ownership error: {error}",
    );

    let mut txn = env.db_txn().await;
    let after = instance.db_instance(&mut txn).await;
    txn.rollback().await.unwrap();
    assert!(after.update_network_config_request.is_none());
}

/// Verifies VPC replacement, VF removal, and instance deletion release generated
/// resources so allocations cannot leak across lifecycle changes.

#[crate::sqlx_test]
async fn test_update_instance_config_vpc_prefix_network_update_multidpu(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let _segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

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
    let ip_prefix = "192.1.4.0/25";
    let vpc_id = get_vpc_fixture_id(&env).await;
    let new_vpc_prefix = rpc::forge::VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let response = env
        .api
        .create_vpc_prefix(request)
        .await
        .unwrap()
        .into_inner();

    let network = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: response.id.map(NetworkDetails::VpcPrefixId),
            device: Some("DPU1".to_string()),
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

    let initial_config = rpc::InstanceConfig {
        tenant: Some(fixture_tenant_config()),
        os: Some(initial_os.clone()),
        network: Some(network.clone()),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let initial_metadata = rpc::Metadata {
        name: "Name1".to_string(),
        description: "Desc1".to_string(),
        labels: vec![],
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(initial_config.clone())
        .metadata(initial_metadata.clone())
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().configs_synced(),
        rpc::forge::SyncState::Synced
    );

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    assert_config_equals(instance.config().inner(), &initial_config);
    assert_metadata_equals(instance.metadata(), &initial_metadata);
    let initial_config_version = instance.config_version();
    assert_eq!(initial_config_version.version_nr(), 1);

    let network = rpc::InstanceNetworkConfig {
        interfaces: vec![
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: None,
                network_details: response.id.map(NetworkDetails::VpcPrefixId),
                device: Some("DPU1".to_string()),
                device_instance: 0,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: None,
                network_details: response.id.map(NetworkDetails::VpcPrefixId),
                device: Some("DPU1".to_string()),
                device_instance: 1,
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
    let mut updated_config_1 = initial_config.clone();
    updated_config_1.network = Some(network);
    let updated_metadata_1 = rpc::Metadata {
        name: "Name2".to_string(),
        description: "Desc2".to_string(),
        labels: vec![rpc::forge::Label {
            key: "Key1".to_string(),
            value: None,
        }],
    };

    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                config: Some(updated_config_1.clone()),
                metadata: Some(updated_metadata_1.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_metadata_equals(instance.metadata.as_ref().unwrap(), &updated_metadata_1);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 2);

    assert_eq!(
        instance.status.as_ref().unwrap().configs_synced(),
        rpc::forge::SyncState::Pending
    );

    // SyncState::Synced means network config update is not applicable.
    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::forge::SyncState::Pending
    );
}

#[crate::sqlx_test]
async fn test_update_instance_config_vpc_prefix_network_update_different_prefix_explicit_ip(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let _segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

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

    // Create a VPC prefix
    let ip_prefix = "192.1.4.0/25";
    let vpc_id = get_vpc_fixture_id(&env).await;
    let new_vpc_prefix = rpc::forge::VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let vpc_prefix_1 = env
        .api
        .create_vpc_prefix(request)
        .await
        .unwrap()
        .into_inner();

    // Create an instance with the first VPC prefix
    // but request some random IP.
    // This should fail.
    env.api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: vpc_prefix_1.id.map(NetworkDetails::VpcPrefixId),
                            device: Some("DPU1".to_string()),
                            device_instance: 0,
                            virtual_function_id: None,
                            ip_address: Some("5.5.5.1".to_string()),
                            ipv6_interface_config: None,
                            routing_profile: None,
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
        .unwrap_err();

    // Create an instance with the first VPC prefix
    // but request the DPU side of a /31
    // This should fail.
    env.api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: vpc_prefix_1.id.map(NetworkDetails::VpcPrefixId),
                            device: Some("DPU1".to_string()),
                            device_instance: 0,
                            virtual_function_id: None,
                            ip_address: Some("192.1.4.0".to_string()),
                            ipv6_interface_config: None,
                            routing_profile: None,
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
        .unwrap_err();

    let expected_ip = "192.1.4.1";
    // Create an instance with the first VPC prefix
    // and request the host side of a /31
    // This should pass.
    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: vpc_prefix_1.id.map(NetworkDetails::VpcPrefixId),
                            device: Some("DPU1".to_string()),
                            device_instance: 0,
                            virtual_function_id: None,
                            ip_address: Some(expected_ip.to_string()),
                            ipv6_interface_config: None,
                            routing_profile: None,
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
        .unwrap()
        .into_inner();

    // Move the instance to ready state
    advance_created_instance_into_ready_state(&env, &mh).await;

    // Look up our instance again to get a fresh snapshot.
    let instance = env
        .api
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance.id.unwrap()],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop()
        .unwrap();

    // Check that we're fully synced and ready.
    assert_eq!(
        instance
            .status
            .as_ref()
            .map(|s| s.configs_synced())
            .unwrap(),
        rpc::forge::SyncState::Synced
    );

    let state = instance
        .status
        .as_ref()
        .and_then(|s| s.clone().tenant.as_ref().map(|t| t.state))
        .unwrap();

    assert_eq!(state, rpc::forge::TenantState::Ready as i32);

    // Check that we actually stored the requested IP.
    assert_eq!(
        instance
            .config
            .and_then(|c| c
                .network
                .and_then(|n| n.interfaces.first().and_then(|i| i.ip_address.clone())))
            .unwrap(),
        expected_ip.to_string()
    );

    // Check that we allocated and pretended to configure the requested IP on the DPU.
    assert_eq!(
        instance.status.unwrap().network.unwrap().interfaces[0].addresses[0],
        expected_ip.to_string()
    );

    // Create an additional VPC prefix

    let ip_prefix1 = "192.0.5.0/25";
    let new_vpc_prefix1 = rpc::forge::VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix1.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix1".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };

    let request = Request::new(new_vpc_prefix1);
    let vpc_prefix_2 = env
        .api
        .create_vpc_prefix(request)
        .await
        .unwrap()
        .into_inner();

    let instance_id = instance.id.unwrap();

    // Update the instance to add a new interface config for the second DPU
    // but try to request some random IPs for both interfaces.
    // This should fail.
    let err = env
        .api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(instance_id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 0,
                                virtual_function_id: None,
                                ip_address: Some("5.5.5.5".to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 1,
                                virtual_function_id: None,
                                ip_address: Some("6.6.6.7".to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                        ],
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
        .unwrap_err();
    assert!(err.message().contains("is not contained within"));

    let expected_ip = "192.0.5.11";
    let expected_ip2 = "192.0.5.1";

    // Update the instance to add a new interface config for the second DPU
    // but try to request the same IP for both interfaces.
    // This should fail.
    let err = env
        .api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(instance_id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 0,
                                virtual_function_id: None,
                                ip_address: Some(expected_ip.to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 1,
                                virtual_function_id: None,
                                ip_address: Some(expected_ip.to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                        ],
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
        .unwrap_err();

    assert!(err.message().contains("prefix already exists"));

    // Update the instance to add a new interface config for the second DPU
    // and try to send in a new IP for the first DPU.
    // This should pass.
    // TODO:  Ideally, this should test the first interface getting a new IP from the
    //        prefix it originally had, but an issue prevents it.  See copy_existing_resources
    //        in crates/api-model/src/instance/config/network.rs
    env.api
        .update_instance_config(
            InstanceConfigUpdateRequest::builder()
                .instance_id(instance_id)
                .config(rpc::InstanceConfig {
                    tenant: Some(fixture_tenant_config()),
                    os: Some(initial_os.clone()),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 0,
                                virtual_function_id: None,
                                ip_address: Some(expected_ip.to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                            rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Physical as i32,
                                network_segment_id: None,
                                network_details: vpc_prefix_2.id.map(NetworkDetails::VpcPrefixId),
                                device: Some("DPU1".to_string()),
                                device_instance: 1,
                                virtual_function_id: None,
                                ip_address: Some(expected_ip2.to_string()),
                                ipv6_interface_config: None,
                                routing_profile: None,
                            },
                        ],
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
        .unwrap()
        .into_inner();

    // Move the instance to ready state after the network config update.
    env.run_machine_state_controller_iteration_network_config_return_to_ready(&mh, true)
        .await;

    // Look up our instance again to get a fresh snapshot.
    let instance = env
        .api
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance_id],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop()
        .unwrap();

    // Check that we're fully synced and ready.
    assert_eq!(
        instance
            .status
            .as_ref()
            .map(|s| s.configs_synced())
            .unwrap(),
        rpc::forge::SyncState::Synced
    );

    let state = instance
        .status
        .as_ref()
        .and_then(|s| s.clone().tenant.as_ref().map(|t| t.state))
        .unwrap();

    assert_eq!(state, rpc::forge::TenantState::Ready as i32);

    // Check that we still correctly stored the requested IP for the first interface
    assert_eq!(
        instance
            .config
            .as_ref()
            .and_then(|c| c
                .network
                .as_ref()
                .and_then(|n| n.interfaces.first().and_then(|i| i.ip_address.clone())))
            .unwrap(),
        expected_ip.to_string()
    );

    // Check that we actually stored the requested IP for the second interface
    assert_eq!(
        instance
            .config
            .as_ref()
            .and_then(|c| c
                .network
                .as_ref()
                .and_then(|n| n.interfaces.last().and_then(|i| i.ip_address.clone())))
            .unwrap(),
        expected_ip2.to_string()
    );

    // Check that we still have the IP we expect for the first interface.
    assert_eq!(
        instance
            .status
            .as_ref()
            .and_then(|s| s
                .network
                .as_ref()
                .and_then(|n| n.interfaces.first().map(|i| i.addresses[0].clone())))
            .unwrap(),
        expected_ip.to_string()
    );

    // Check that we actually _received_ the requested IP on the second interface.
    assert_eq!(
        instance
            .status
            .as_ref()
            .and_then(|s| s
                .network
                .as_ref()
                .and_then(|n| n.interfaces.last().map(|i| i.addresses[0].clone())))
            .unwrap(),
        expected_ip2.to_string()
    );
}
