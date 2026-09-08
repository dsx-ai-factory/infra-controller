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
use std::ops::DerefMut;

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_test_support::Outcome::FailsWith;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::vpc::VpcId;
use common::api_fixtures::{create_test_env, populate_network_security_groups};
use config_version::ConfigVersion;
use db::vpc::{self};
use db::{self, ObjectColumnFilter};
use model::metadata::Metadata;
use model::vpc::{
    NewVpc, PowerResourceGroupUpdate, UpdateVpc, UpdateVpcVirtualization, VpcDefinition,
    VpcRoutingProfileOverrides, VpcStatus,
};
use rpc::forge::forge_server::Forge;

use crate::test_support::metadata;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common;
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};
use crate::tests::common::rpc_builder::{VpcCreationRequest, VpcDeletionRequest, VpcUpdateRequest};
use crate::{DatabaseError, db_init};

fn forge_vpc_config(vpc: &rpc::forge::Vpc) -> &rpc::forge::VpcConfig {
    vpc.config
        .as_ref()
        .expect("structured config must be populated")
}

#[crate::sqlx_test]
async fn create_vpc_for_tenant_without_profile(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Create a tenant.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "sizzle".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "sizzle".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    // Try to request a VPC without sending a valid tenant org. Routing-
    // profile validation lives behind the FNN-only path, so the request
    // has to be an FNN VPC to exercise the "no tenant" branch.
    assert!(
        env.api
            .create_vpc(
                VpcCreationRequest::builder("")
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("PRIVILEGED_INTERNAL".to_string())
                    .tonic_request(),
            )
            .await
            .unwrap_err()
            .message()
            .contains("no tenant or routing profile type found")
    );

    // Try to request a VPC with a routing profile when the tenant has no routing profile type
    assert!(
        env.api
            .create_vpc(
                VpcCreationRequest::builder(tenant.organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("PRIVILEGED_INTERNAL".to_string())
                    .tonic_request(),
            )
            .await
            .unwrap_err()
            .message()
            .contains("no tenant or routing profile type found")
    );

    Ok(())
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn create_vpc(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    // Build an FNN config with distinct access tiers so the create path
    // covers the new routing-profile validation.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_fnn_config(Some(crate::cfg::file::FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([
                (
                    "INTERNAL".to_string(),
                    crate::cfg::file::FnnRoutingProfileConfig {
                        internal: Some(true),
                        access_tier: Some(1),
                        ..Default::default()
                    },
                ),
                (
                    "PRIVILEGED_INTERNAL".to_string(),
                    crate::cfg::file::FnnRoutingProfileConfig {
                        internal: Some(true),
                        access_tier: Some(0),
                        ..Default::default()
                    },
                ),
            ]),
            use_vpc_vrf_loopback: false,
        })),
    )
    .await;

    // Create a tenant using the current string field.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "sizzle".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "sizzle".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    // Try to request a VNI that shouldn't exist
    // (based on VPC_VNI pool definition in pool_defs in crates/api/src/tests/common/api_fixtures/mod.rs)
    assert!(
        env.api
            .create_vpc(
                VpcCreationRequest::builder(&tenant.organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .vni(100u32)
                    .tonic_request(),
            )
            .await
            .unwrap_err()
            .message()
            .contains("cannot be requested")
    );

    // Try to request a VNI that shouldn't be available.
    // This should fail.
    assert!(
        env.api
            .create_vpc(
                VpcCreationRequest::builder(&tenant.organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .vni(20002u32)
                    .tonic_request(),
            )
            .await
            .unwrap_err()
            .message()
            .contains("cannot be requested")
    );

    // Create another tenant.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "fizzle".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "fizzle".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    // Try to request a broader routing profile for the VPC. Access-tier
    // broadening checks live behind the routing-profile path, which is
    // FNN-only -- the request has to be an FNN VPC to even reach that
    // validation. This should fail.
    assert!(
        env.api
            .create_vpc(
                VpcCreationRequest::builder(&tenant.organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("PRIVILEGED_INTERNAL".to_string())
                    .tonic_request(),
            )
            .await
            .unwrap_err()
            .message()
            .contains("broader than associated tenant routing-profile access tier")
    );

    // Create a VPC by explicitly selecting a VNI from
    // the allowed pool.
    let forge_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(&tenant.organization_id)
                .vni(60001u32)
                .metadata(rpc::forge::Metadata {
                    name: "Forge_with_vni".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // A VNI is allocated
    assert!(forge_vpc.status.as_ref().and_then(|s| s.vni).is_some());
    // The 'config' VNI and the status VNI match
    assert_eq!(
        forge_vpc_config(&forge_vpc).vni,
        forge_vpc.status.as_ref().and_then(|s| s.vni)
    );

    // Create another VPC by explicitly selecting a VNI from
    // the allowed pool, but use the same VNI, so it should fail.
    let _ = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(&tenant.organization_id)
                .vni(60001u32)
                .metadata(rpc::forge::Metadata {
                    name: "Forge_with_vni_dupe".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Clean it up so the rest of our tests can work with a single VPC in the DB.
    env.api
        .delete_vpc(
            VpcDeletionRequest::builder()
                .id(forge_vpc.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // No network_virtualization_type, should default
    let forge_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(&tenant.organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "Forge".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let version: ConfigVersion = forge_vpc.version.parse()?;
    assert_eq!(version.version_nr(), 1);
    // A VNI is allocated
    assert!(forge_vpc.status.as_ref().and_then(|s| s.vni).is_some());
    // The 'config' VNI is still None because this was an auto-allocated VNI
    assert!(forge_vpc_config(&forge_vpc).vni.is_none());
    // We default to EthernetVirtualizer (proto value 0).
    assert_eq!(
        forge_vpc_config(&forge_vpc).network_virtualization_type,
        Some(0)
    );

    let no_org_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(&tenant.organization_id)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::from(
                    VpcVirtualizationType::EthernetVirtualizer,
                ))
                .metadata(Metadata {
                    name: "Forge no Org".to_string(),
                    ..Metadata::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();
    let no_org_vpc_version: ConfigVersion = no_org_vpc.version.parse()?;
    assert_eq!(no_org_vpc_version.version_nr(), 1);

    assert!(no_org_vpc.deleted.is_none());
    let initial_no_org_vpc_version = no_org_vpc_version;

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let no_org_vpc_id: VpcId = no_org_vpc.id.expect("should have id");

    // A power-resource-group-only update still validates that the VPC exists.
    let unknown_vpc_id = VpcId::from(uuid::Uuid::new_v4());
    let status = env
        .api
        .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
            id: Some(unknown_vpc_id),
            if_version_match: None,
            metadata: None,
            network_security_group_id: None,
            default_nvlink_logical_partition_id: None,
            routing_profile_overrides: None,
            power_resource_group: Some("power-group".to_string()),
        }))
        .await
        .expect_err("updating an unknown VPC should fail");
    assert_eq!(status.code(), tonic::Code::NotFound);

    // Try to update to invalid metadata
    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(true) {
        let invalid_updated_vpc = env
            .api
            .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
                id: Some(no_org_vpc_id),
                if_version_match: None,
                metadata: Some(invalid_metadata.clone()),
                network_security_group_id: None,
                default_nvlink_logical_partition_id: None,
                routing_profile_overrides: None,
                power_resource_group: None,
            }))
            .await;

        let err = invalid_updated_vpc.expect_err(&format!(
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

    let updated_metadata = Metadata {
        name: "new name".to_string(),
        description: "".to_string(),
        labels: HashMap::from([("label_new_key".to_string(), "label_new_value".to_string())]),
    };

    let updated_vpc = db::vpc::update(
        &UpdateVpc {
            id: no_org_vpc_id,
            if_version_match: None,
            metadata: updated_metadata.clone(),
            network_security_group_id: None,
            routing_profile_overrides: None,
            power_resource_group: Some(PowerResourceGroupUpdate::Set("power-group".to_string())),
        },
        &mut txn,
    )
    .await?;

    assert_eq!(updated_vpc.metadata, updated_metadata);
    assert_eq!(
        updated_vpc.config.power_resource_group.as_deref(),
        Some("power-group")
    );
    assert_eq!(updated_vpc.version.version_nr(), 2);

    // DB value "etv" decodes as EthernetVirtualizer.
    assert_eq!(
        updated_vpc.config.network_virtualization_type,
        VpcVirtualizationType::EthernetVirtualizer
    );

    // Update virtualization type.
    let orig_virtualization_type = updated_vpc.config.network_virtualization_type;
    let _updated_vpc_virtualization = db::vpc::update_virtualization(
        &UpdateVpcVirtualization {
            id: no_org_vpc_id,
            if_version_match: None,
            network_virtualization_type: VpcVirtualizationType::Fnn,
        },
        &mut txn,
    )
    .await?;

    let mut vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &no_org_vpc_id),
    )
    .await?;
    let first = vpcs.swap_remove(0);
    assert_eq!(
        first.config.network_virtualization_type,
        VpcVirtualizationType::Fnn
    );

    // And then put the virtualization type back and mark
    // this as the latest `updated_vpc` for subsequent checks.
    let updated_vpc = db::vpc::update_virtualization(
        &UpdateVpcVirtualization {
            id: no_org_vpc_id,
            if_version_match: None,
            network_virtualization_type: orig_virtualization_type,
        },
        &mut txn,
    )
    .await?;

    let mut vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &no_org_vpc_id),
    )
    .await?;
    let first = vpcs.swap_remove(0);
    assert_eq!(
        first.config.network_virtualization_type,
        VpcVirtualizationType::EthernetVirtualizer
    );

    // Update on outdated version
    let update_result = db::vpc::update(
        &UpdateVpc {
            id: no_org_vpc_id,
            if_version_match: Some(initial_no_org_vpc_version),
            network_security_group_id: None,
            routing_profile_overrides: None,
            power_resource_group: None,
            metadata: Metadata {
                name: "never this name".to_string(),
                description: "".to_string(),
                labels: HashMap::new(),
            },
        },
        &mut txn,
    )
    .await;
    assert!(matches!(
        update_result,
        Err(DatabaseError::ConcurrentModificationError(_, _))
    ));

    // Check that the data was indeed not touched
    let mut vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &no_org_vpc_id),
    )
    .await?;
    let first = vpcs.swap_remove(0);
    assert_eq!(&first.metadata.name, "new name");
    assert_eq!(first.version.version_nr(), 4); // includes 2 changes to VPC virtualization type

    // Update on correct version
    let updated_vpc = db::vpc::update(
        &UpdateVpc {
            id: no_org_vpc_id,
            network_security_group_id: None,
            routing_profile_overrides: None,
            power_resource_group: None,
            if_version_match: Some(updated_vpc.version),
            metadata: Metadata {
                name: "yet another new name".to_string(),
                description: "".to_string(),
                labels: HashMap::new(),
            },
        },
        &mut txn,
    )
    .await?;
    assert_eq!(&updated_vpc.metadata.name, "yet another new name");
    assert_eq!(updated_vpc.version.version_nr(), 5);
    assert_eq!(
        updated_vpc.config.power_resource_group.as_deref(),
        Some("power-group")
    );

    let updated_vpc = db::vpc::update(
        &UpdateVpc {
            id: no_org_vpc_id,
            network_security_group_id: None,
            routing_profile_overrides: None,
            power_resource_group: Some(PowerResourceGroupUpdate::Clear),
            if_version_match: Some(updated_vpc.version),
            metadata: updated_vpc.metadata.clone(),
        },
        &mut txn,
    )
    .await?;
    assert_eq!(updated_vpc.config.power_resource_group, None);
    assert_eq!(updated_vpc.version.version_nr(), 6);

    let mut vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &no_org_vpc_id),
    )
    .await?;
    let first = vpcs.swap_remove(0);
    assert_eq!(&first.metadata.name, "yet another new name");
    assert_eq!(first.version.version_nr(), 6);
    assert_eq!(first.config.power_resource_group, None);

    let vpcs = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &no_org_vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?;
    assert_eq!(vpcs.len(), 1);
    let vpc = db::vpc::try_delete(&mut txn, no_org_vpc_id).await?.unwrap();

    assert!(vpc.deleted.is_some());

    let vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &vpc.id),
    )
    .await?;

    txn.commit().await?;

    assert!(vpcs.is_empty());

    let mut txn = env.pool.begin().await?;
    let vpcs = db::vpc::find_by(txn.as_mut(), ObjectColumnFilter::<vpc::IdColumn>::All).await?;
    assert_eq!(vpcs.len(), 1);
    let forge_vpc_id: VpcId = forge_vpc.id.expect("should have id");
    assert_eq!(vpcs[0].id, forge_vpc_id);

    let vpcs = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &forge_vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?;
    assert_eq!(vpcs.len(), 1);
    let vpc = db::vpc::try_delete(&mut txn, forge_vpc_id).await?.unwrap();
    assert!(vpc.deleted.is_some());
    txn.commit().await?;

    let mut txn = env.pool.begin().await?;
    let vpcs = db::vpc::find_by(txn.as_mut(), ObjectColumnFilter::<vpc::IdColumn>::All).await?;
    assert!(vpcs.is_empty());
    txn.commit().await?;

    Ok(())
}

/// Verifies both creation and update reject routing-profile properties for
/// VPC types whose data plane cannot apply them.
#[crate::sqlx_test]
async fn vpc_without_fnn_rejects_routing_profile_fields(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        },
    )
    .await;

    // Seed a tenant directly so the no-FNN VPC path sees a stored profile.
    let tenant_organization_id = "sizzle_without_fnn".to_string();
    {
        let mut txn = env.pool.begin().await?;
        db::tenant::create_and_persist(
            tenant_organization_id.clone(),
            Metadata {
                name: "sizzle_without_fnn".to_string(),
                description: "".to_string(),
                labels: HashMap::new(),
            },
            Some("INTERNAL".to_string()),
            txn.deref_mut(),
        )
        .await?;
        txn.commit().await?;
    };

    // Requesting either VPC routing-profile field on a non-FNN VPC type
    // (default is ETV) should fail early at the API gate. The REST API
    // enforces this upstream; carbide-core enforces it as defense-in-depth
    // via `ensure_supports_routing_profiles`.
    check_cases_async(
        [
            Case {
                scenario: "routing_profile_type",
                input: VpcCreationRequest::builder(&tenant_organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .routing_profile_type("PRIVILEGED_INTERNAL".to_string())
                    .tonic_request(),
                expect: FailsWith(true),
            },
            Case {
                scenario: "routing_profile_overrides",
                input: VpcCreationRequest::builder(&tenant_organization_id)
                    .metadata(rpc::forge::Metadata {
                        name: "Forge".to_string(),
                        ..Default::default()
                    })
                    .routing_profile_overrides(rpc::forge::VpcRoutingProfileOverrides {
                        leak_default_route_from_underlay: Some(false),
                        ..Default::default()
                    })
                    .tonic_request(),
                expect: FailsWith(true),
            },
        ],
        |request| {
            let api = env.api.clone();
            async move {
                api.create_vpc(request)
                    .await
                    .map(drop)
                    .map_err(|error| {
                        error.message().contains(
                            "`routing_profile_type` and `routing_profile_overrides` fields are FNN-only",
                        )
                    })
            }
        },
    )
    .await;

    // Create an ordinary non-FNN VPC so the update path can be checked.
    let created = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(&tenant_organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "non-FNN update test".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner();
    let vpc_id = created.id.expect("created VPC ID");

    // An inline update cannot attach routing behavior to an unsupported VPC.
    let error = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(Some(vpc_id))
                .routing_profile_overrides(rpc::forge::VpcRoutingProfileOverrides {
                    leak_default_route_from_underlay: Some(false),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .expect_err("non-FNN routing-profile update should fail");
    assert!(
        error
            .message()
            .contains("`routing_profile_type` and `routing_profile_overrides` fields are FNN-only"),
        "unexpected error: {error}"
    );

    // Read through the public API to prove the rejected value was not persisted.
    let found = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted VPC");
    assert!(forge_vpc_config(&found).routing_profile_overrides.is_none());

    Ok(())
}

/// SLAAC is immutable VPC creation policy and is supported only by FNN.
#[crate::sqlx_test]
async fn test_slaac_vpc_creation_contract(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let tenant_organization_id = "slaac-vpc-tenant";
    create_fixture_tenant(&env, tenant_organization_id).await?;

    check_cases_async(
        [
            Case {
                scenario: "default Ethernet virtualization",
                input: VpcCreationRequest::builder(tenant_organization_id)
                    .metadata(Metadata {
                        name: "SLAAC on ETV".to_string(),
                        ..Default::default()
                    })
                    .slaac_enabled(true)
                    .tonic_request(),
                expect: FailsWith(true),
            },
            Case {
                scenario: "Flat virtualization",
                input: VpcCreationRequest::builder(tenant_organization_id)
                    .metadata(Metadata {
                        name: "SLAAC on Flat".to_string(),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Flat as i32)
                    .slaac_enabled(true)
                    .tonic_request(),
                expect: FailsWith(true),
            },
        ],
        |request| {
            let api = env.api.clone();
            async move {
                api.create_vpc(request).await.map(drop).map_err(|error| {
                    error.code() == tonic::Code::InvalidArgument
                        && error
                            .message()
                            .contains("VPCs do not support SLAAC allocation mode")
                })
            }
        },
    )
    .await;

    let omitted = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(Metadata {
                    name: "FNN without SLAAC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    assert_eq!(forge_vpc_config(&omitted).slaac_enabled, Some(false));

    let explicitly_disabled = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(Metadata {
                    name: "FNN with SLAAC explicitly disabled".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .slaac_enabled(false)
                .tonic_request(),
        )
        .await?
        .into_inner();
    assert_eq!(
        forge_vpc_config(&explicitly_disabled).slaac_enabled,
        Some(false)
    );

    // VPC creation succeeds before any IPv6 (or other) VPC prefix exists.
    let created = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(Metadata {
                    name: "SLAAC on FNN".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .slaac_enabled(true)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let vpc_id = created.id.expect("created VPC ID");
    assert_eq!(forge_vpc_config(&created).slaac_enabled, Some(true));

    let mut txn = env.pool.begin().await?;
    assert!(
        db::vpc_prefix::find_by_vpc(txn.as_mut(), vpc_id)
            .await?
            .is_empty()
    );
    let persisted = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &vpc_id),
    )
    .await?
    .pop()
    .expect("persisted SLAAC VPC");
    assert!(persisted.config.slaac_enabled);
    txn.commit().await?;

    // The separate virtualization update API cannot violate the rule that
    // SLAAC is supported only for FNN because `slaac_enabled` is fixed when the
    // VPC is created.
    let error = env
        .api
        .update_vpc_virtualization(tonic::Request::new(
            rpc::forge::VpcUpdateVirtualizationRequest {
                id: Some(vpc_id),
                if_version_match: None,
                network_virtualization_type: Some(
                    rpc::forge::VpcVirtualizationType::EthernetVirtualizer as i32,
                ),
            },
        ))
        .await
        .expect_err("a SLAAC VPC must remain FNN");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("VPCs do not support SLAAC allocation mode")
    );

    Ok(())
}

/// Verifies override updates require a currently resolvable named base so the
/// API cannot persist policy that the FNN data plane is unable to render.
#[crate::sqlx_test]
async fn update_vpc_rejects_unresolvable_routing_profile_base(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;

    // Create through the supported missing-tenant fallback, which leaves an
    // FNN VPC without a named routing profile.
    let profileless = env
        .api
        .create_vpc(
            VpcCreationRequest::builder("profileless-vpc")
                .metadata(rpc::forge::Metadata {
                    name: "profileless VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let profileless_id = profileless.id.expect("profileless VPC ID");

    // Model configuration drift directly because runtime configuration is
    // immutable after the test API starts and the database intentionally has
    // no constraint tying profile names to that configuration.
    let stale_profile_id = VpcId::new();
    let mut txn = env.pool.begin().await?;
    let stale_vpc = db::vpc::persist(
        NewVpc {
            id: stale_profile_id,
            tenant_organization_id: "stale-profile-vpc".to_string(),
            network_virtualization_type: VpcVirtualizationType::Fnn,
            metadata: Metadata {
                name: "stale profile VPC".to_string(),
                ..Default::default()
            },
            network_security_group_id: None,
            routing_profile_type: Some("REMOVED_PROFILE".to_string()),
            routing_profile_overrides: None,
            power_resource_group: Some("stale-power-group".to_string()),
            vni: None,
            slaac_enabled: false,
        },
        VpcStatus { vni: None },
        &mut txn,
    )
    .await?;
    assert_eq!(
        stale_vpc.config.power_resource_group.as_deref(),
        Some("stale-power-group")
    );
    txn.commit().await?;

    check_cases_async(
        [
            // A profile-less FNN VPC has no base onto which an override can
            // safely be applied.
            Case {
                scenario: "missing named base profile",
                input: profileless_id,
                expect: FailsWith(tonic::Code::FailedPrecondition),
            },
            // A persisted name removed from runtime configuration must not
            // silently produce an ineffective override.
            Case {
                scenario: "named base removed from runtime configuration",
                input: stale_profile_id,
                expect: FailsWith(tonic::Code::NotFound),
            },
        ],
        |vpc_id| {
            let api = env.api.clone();
            async move {
                api.update_vpc(
                    VpcUpdateRequest::builder()
                        .set_id(Some(vpc_id))
                        .routing_profile_overrides(rpc::forge::VpcRoutingProfileOverrides {
                            leak_default_route_from_underlay: Some(false),
                            ..Default::default()
                        })
                        .tonic_request(),
                )
                .await
                .map(drop)
                .map_err(|error| error.code())
            }
        },
    )
    .await;

    // Read through the public API to prove neither rejected definition was
    // persisted.
    for (scenario, vpc_id) in [
        ("missing named base profile", profileless_id),
        (
            "named base removed from runtime configuration",
            stale_profile_id,
        ),
    ] {
        let found = env
            .api
            .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
            .await?
            .into_inner()
            .vpcs
            .pop()
            .expect("persisted VPC");
        assert!(
            forge_vpc_config(&found).routing_profile_overrides.is_none(),
            "{scenario} unexpectedly persisted routing-profile overrides"
        );
    }

    Ok(())
}

/// Verifies inline routing-profile values survive persistence, omitted update
/// fields preserve them, and present definitions replace them while inheriting
/// unset properties from the base profile. An explicitly empty definition
/// restores full inheritance, preventing unrelated updates from erasing policy
/// or replacements from retaining stale values.
#[crate::sqlx_test]
async fn vpc_routing_profile_overrides_can_be_updated(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile_type = "INLINE_PROFILE_TEST";

    // Configure the named profile that provides the VPC's protected base.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default().with_fnn_config(Some(crate::cfg::file::FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([(
                profile_type.to_string(),
                crate::cfg::file::FnnRoutingProfileConfig {
                    route_target_imports: Some(vec![crate::cfg::file::RouteTargetConfig {
                        asn: 65000,
                        vni: 100,
                    }]),
                    internal: Some(true),
                    leak_default_route_from_underlay: Some(true),
                    allowed_anycast_prefixes: Some(vec![
                        crate::cfg::file::PrefixFilterPolicyEntry {
                            prefix: "198.51.100.0/24".parse()?,
                        },
                    ]),
                    access_tier: Some(1),
                    ..Default::default()
                },
            )]),
            use_vpc_vrf_loopback: false,
        })),
    )
    .await;
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "inline-profile-test".to_string(),
            routing_profile_type: Some(profile_type.to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "inline-profile-test".to_string(),
                ..Default::default()
            }),
        }))
        .await?
        .into_inner()
        .tenant
        .expect("created tenant");
    let routing_profile_overrides = rpc::forge::VpcRoutingProfileOverrides {
        route_target_imports: Some(rpc::common::RouteTargets { values: vec![] }),
        leak_default_route_from_underlay: Some(false),
        allowed_anycast_prefixes: Some(rpc::forge::PrefixFilterPolicyEntries {
            values: vec![rpc::forge::PrefixFilterPolicyEntry {
                prefix: "192.0.2.0/24".to_string(),
            }],
        }),
        ..Default::default()
    };
    let expected_effective_profile = rpc::forge::VpcEffectiveRoutingProfile {
        route_target_imports: vec![],
        leak_default_route_from_underlay: false,
        allowed_anycast_prefixes: vec![rpc::forge::PrefixFilterPolicyEntry {
            prefix: "192.0.2.0/24".to_string(),
        }],
        internal: true,
        access_tier: 1,
        ..Default::default()
    };

    // Create the VPC and verify the API echoes the presence-aware override
    // alongside the profile resolved from the current API configuration.
    let created = env
        .api
        .create_vpc(tonic::Request::new(
            VpcCreationRequest::builder(&tenant.organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "inline profile vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type(profile_type.to_string())
                .routing_profile_overrides(routing_profile_overrides.clone())
                .rpc(),
        ))
        .await?
        .into_inner();
    assert_eq!(
        forge_vpc_config(&created).routing_profile_overrides,
        Some(routing_profile_overrides.clone())
    );
    assert_eq!(
        created
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&expected_effective_profile)
    );

    // Read through the public find API to prove the override was persisted.
    let found = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![created.id.expect("created VPC ID")],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted VPC");
    assert_eq!(
        forge_vpc_config(&found).routing_profile_overrides,
        Some(routing_profile_overrides.clone())
    );
    assert_eq!(
        found
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&expected_effective_profile)
    );

    // Updating unrelated VPC configuration must return the same current
    // effective profile as create and find.
    let updated = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(found.id)
                .metadata(rpc::forge::Metadata {
                    name: "updated inline profile vpc".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner()
        .vpc
        .expect("updated VPC");
    assert_eq!(
        forge_vpc_config(&updated).routing_profile_overrides,
        Some(routing_profile_overrides)
    );
    assert_eq!(
        updated
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&expected_effective_profile)
    );

    let updated_id = updated.id.expect("updated VPC ID");
    let updated_routing_profile_overrides = rpc::forge::VpcRoutingProfileOverrides {
        route_targets_on_exports: Some(rpc::common::RouteTargets {
            values: vec![rpc::common::RouteTarget {
                asn: 65100,
                vni: 200,
            }],
        }),
        leak_tenant_host_routes_to_underlay: Some(true),
        ..Default::default()
    };
    let updated_effective_profile = rpc::forge::VpcEffectiveRoutingProfile {
        route_target_imports: vec![rpc::common::RouteTarget {
            asn: 65000,
            vni: 100,
        }],
        route_targets_on_exports: vec![rpc::common::RouteTarget {
            asn: 65100,
            vni: 200,
        }],
        leak_default_route_from_underlay: true,
        leak_tenant_host_routes_to_underlay: true,
        allowed_anycast_prefixes: vec![rpc::forge::PrefixFilterPolicyEntry {
            prefix: "198.51.100.0/24".to_string(),
        }],
        internal: true,
        access_tier: 1,
        ..Default::default()
    };

    // Replace the inline definition. Properties omitted from the new message
    // must inherit from the base rather than retain the previous overrides.
    let updated = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(Some(updated_id))
                .metadata(rpc::forge::Metadata {
                    name: "updated inline profile vpc".to_string(),
                    ..Default::default()
                })
                .routing_profile_overrides(updated_routing_profile_overrides.clone())
                .tonic_request(),
        )
        .await?
        .into_inner()
        .vpc
        .expect("updated VPC");
    assert_eq!(
        forge_vpc_config(&updated).routing_profile_overrides,
        Some(updated_routing_profile_overrides.clone())
    );
    assert_eq!(
        updated
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&updated_effective_profile)
    );

    // Read through the public API to prove both the replacement definition and
    // its newly resolved effective profile were persisted.
    let found = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![updated_id],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted updated VPC");
    assert_eq!(
        forge_vpc_config(&found).routing_profile_overrides,
        Some(updated_routing_profile_overrides)
    );
    assert_eq!(
        found
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&updated_effective_profile)
    );

    let empty_routing_profile_overrides = rpc::forge::VpcRoutingProfileOverrides::default();
    let inherited_effective_profile = rpc::forge::VpcEffectiveRoutingProfile {
        route_target_imports: vec![rpc::common::RouteTarget {
            asn: 65000,
            vni: 100,
        }],
        leak_default_route_from_underlay: true,
        allowed_anycast_prefixes: vec![rpc::forge::PrefixFilterPolicyEntry {
            prefix: "198.51.100.0/24".to_string(),
        }],
        internal: true,
        access_tier: 1,
        ..Default::default()
    };

    // An explicitly empty definition replaces all prior inline properties,
    // restoring inheritance from the named base profile.
    let reset = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(Some(updated_id))
                .metadata(rpc::forge::Metadata {
                    name: "updated inline profile vpc".to_string(),
                    ..Default::default()
                })
                .routing_profile_overrides(empty_routing_profile_overrides.clone())
                .tonic_request(),
        )
        .await?
        .into_inner()
        .vpc
        .expect("reset VPC");
    assert_eq!(
        forge_vpc_config(&reset).routing_profile_overrides,
        Some(empty_routing_profile_overrides.clone())
    );
    assert_eq!(
        reset
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&inherited_effective_profile)
    );

    // Read through the public API to prove the empty definition remains
    // present and the fully inherited effective profile was persisted.
    let found = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![updated_id],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted reset VPC");
    assert_eq!(
        forge_vpc_config(&found).routing_profile_overrides,
        Some(empty_routing_profile_overrides)
    );
    assert_eq!(
        found
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref()),
        Some(&inherited_effective_profile)
    );

    // The virtualization update API currently permits this transition even
    // though callers are instructed not to use it. A non-FNN VPC must not
    // report an effective routing profile retained from its former FNN state.
    env.api
        .update_vpc_virtualization(tonic::Request::new(
            rpc::forge::VpcUpdateVirtualizationRequest {
                id: Some(updated_id),
                if_version_match: None,
                network_virtualization_type: Some(rpc::forge::VpcVirtualizationType::Flat as i32),
            },
        ))
        .await?;
    let transitioned = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![updated_id],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("transitioned VPC");
    assert!(
        transitioned
            .status
            .as_ref()
            .and_then(|status| status.effective_routing_profile.as_ref())
            .is_none()
    );

    Ok(())
}

/// Verifies seeded inline overrides are rejected even for existing VPCs
/// because idempotent seed handling must not hide unsupported configuration.
#[crate::sqlx_test]
async fn initial_vpc_inline_overrides_are_rejected_before_existing_vpc_check(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;
    let vpc_name = "seeded-inline-profile";

    // Seed a valid VPC so the rejected definition exercises the existing-VPC path.
    let initial_vpcs = HashMap::from([(
        vpc_name.to_string(),
        VpcDefinition {
            organization_id: Some(FIXTURE_TENANT_ORG_ID.to_string()),
            network_virtualization_type: VpcVirtualizationType::Fnn,
            routing_profile_type: Some("BASE".to_string()),
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    db_init::create_initial_vpcs(
        &env.pool,
        &initial_vpcs,
        env.common_pools.ethernet.pool_vpc_vni.as_ref(),
    )
    .await?;

    // Re-submit the same name with an override and a valid-looking named base.
    let vpcs = HashMap::from([(
        vpc_name.to_string(),
        VpcDefinition {
            organization_id: Some(FIXTURE_TENANT_ORG_ID.to_string()),
            network_virtualization_type: VpcVirtualizationType::Fnn,
            routing_profile_type: Some("BASE".to_string()),
            routing_profile_overrides: Some(VpcRoutingProfileOverrides {
                leak_default_route_from_underlay: Some(true),
                ..Default::default()
            }),
            vni: None,
        },
    )]);

    // Reject the unsupported override before the existing-VPC short-circuit.
    let error = db_init::create_initial_vpcs(
        &env.pool,
        &vpcs,
        env.common_pools.ethernet.pool_vpc_vni.as_ref(),
    )
    .await
    .expect_err("seeded inline overrides must always be rejected");
    assert!(
        error
            .to_string()
            .contains("cannot define `routing_profile_overrides`"),
        "unexpected error: {error}"
    );

    // Read through the public API and verify the original VPC remains unchanged.
    let found_ids = env
        .api
        .find_vpc_ids(tonic::Request::new(rpc::forge::VpcSearchFilter {
            name: Some(vpc_name.to_string()),
            tenant_org_id: None,
            label: None,
        }))
        .await?
        .into_inner()
        .vpc_ids;
    assert_eq!(found_ids.len(), 1);

    let found = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: found_ids,
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted VPC");
    assert!(forge_vpc_config(&found).routing_profile_overrides.is_none());

    Ok(())
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn create_vpc_with_labels(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let forge_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder("Forge_unit_tests")
                .metadata(Metadata {
                    name: "test_VPC_with_labels".to_string(),
                    description: "this VPC must have labels.".to_string(),
                    labels: vec![("key1", "value1"), ("key2", "")]
                        .into_iter()
                        .map(|(k, v)| (k.into(), v.into()))
                        .collect(),
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let vpc_id: VpcId = forge_vpc.id.expect("should have id");

    assert_eq!(
        &forge_vpc.metadata.clone().unwrap().name,
        "test_VPC_with_labels"
    );
    assert_eq!(
        forge_vpc.metadata.clone().unwrap().description,
        "this VPC must have labels."
    );
    assert!(forge_vpc.metadata.clone().unwrap().labels.len() == 2);

    assert_eq!(
        forge_vpc
            .metadata
            .clone()
            .unwrap()
            .labels
            .iter()
            .find(|label| label.key == "key1")
            .and_then(|label| label.value.as_deref()),
        Some("value1")
    );

    assert_eq!(
        forge_vpc
            .metadata
            .clone()
            .unwrap()
            .labels
            .iter()
            .find(|label| label.key == "key2")
            .and_then(|label| label.value.as_deref()),
        None
    );

    let request_vpcs = tonic::Request::new(rpc::forge::VpcsByIdsRequest {
        vpc_ids: vec![vpc_id],
    });

    let vpc_list = env
        .api
        .find_vpcs_by_ids(request_vpcs)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(vpc_list.vpcs.len(), 1);
    let fetched_vpc = vpc_list.vpcs[0].clone();

    assert_eq!(
        &fetched_vpc.metadata.clone().unwrap().name,
        "test_VPC_with_labels"
    );
    assert_eq!(
        &fetched_vpc
            .config
            .as_ref()
            .expect("config")
            .tenant_organization_id,
        "Forge_unit_tests"
    );
    assert_eq!(
        fetched_vpc.metadata.clone().unwrap().description,
        "this VPC must have labels."
    );
    assert!(fetched_vpc.metadata.clone().unwrap().labels.len() == 2);

    assert_eq!(
        fetched_vpc
            .metadata
            .clone()
            .unwrap()
            .labels
            .iter()
            .find(|label| label.key == "key1")
            .and_then(|label| label.value.as_deref()),
        Some("value1")
    );

    assert_eq!(
        fetched_vpc
            .metadata
            .unwrap()
            .labels
            .iter()
            .find(|label| label.key == "key2")
            .and_then(|label| label.value.as_deref()),
        None
    );

    Ok(())
}

#[crate::sqlx_test]
async fn create_vpc_with_invalid_metadata(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(true) {
        let result = env
            .api
            .create_vpc(
                VpcCreationRequest::builder("Forge_unit_tests")
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
        )
    }

    Ok(())
}

#[crate::sqlx_test]
async fn find_vpc_by_id(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let vpc_id = VpcId::new();

    sqlx::query(r#"
        INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, 'test vpc 1', $2, 'V1-T1666644937952267');
    "#).bind(vpc_id).bind(FIXTURE_TENANT_ORG_ID).execute(txn.deref_mut()).await?;

    let some_vpc = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &vpc_id),
    )
    .await?;
    assert_eq!(1, some_vpc.len());

    let first = some_vpc.first();
    assert!(matches!(first, Some(x) if x.id == vpc_id));

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_with_id(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = VpcId::new();

    // No network_virtualization_type, should default
    let forge_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder("")
                .id(id)
                .metadata(Metadata {
                    name: "Forge".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    assert_eq!(forge_vpc.id.unwrap(), id);
    Ok(())
}

#[crate::sqlx_test]
async fn vpc_deletion_is_idempotent(pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;

    let vpc_req = VpcCreationRequest::builder("test")
        .metadata(Metadata {
            name: "test_vpc".to_string(),
            ..Default::default()
        })
        .tonic_request();
    let resp = env.api.create_vpc(vpc_req).await.unwrap().into_inner();

    let vpc_id = resp.id.unwrap();
    assert_eq!(resp.metadata.unwrap().name, "test_vpc");

    let vpc_list = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await
        .unwrap()
        .into_inner();

    let vpc_name = vpc_list.vpcs[0].metadata.as_ref().unwrap().name.clone();

    assert_eq!(vpc_list.vpcs.len(), 1);
    assert_eq!(vpc_list.vpcs[0].id, Some(vpc_id));
    assert_eq!(vpc_name, "test_vpc");

    // Delete the first time. Queries should now yield no results
    env.api
        .delete_vpc(tonic::Request::new(rpc::forge::VpcDeletionRequest {
            id: Some(vpc_id),
        }))
        .await
        .unwrap()
        .into_inner();

    let vpc_list = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(vpc_list.vpcs.is_empty());
    let vpc_list = env
        .api
        .find_vpc_ids(tonic::Request::new(rpc::forge::VpcSearchFilter {
            name: Some("test_vpc".to_string()),
            tenant_org_id: None,
            label: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(vpc_list.vpc_ids.is_empty());

    // With a duplicated delete query, we want to return NotFound
    let delete_result = env
        .api
        .delete_vpc(tonic::Request::new(rpc::forge::VpcDeletionRequest {
            id: Some(vpc_id),
        }))
        .await;
    let err = delete_result.expect_err("Deletion should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    assert_eq!(err.message(), format!("vpc not found: {vpc_id}"));

    Ok(())
}

#[crate::sqlx_test]
async fn create_admin_vpc(pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let vni = 10000;
    db_init::create_admin_vpc(&env.pool, Some(vni)).await?;

    let mut txn = env.pool.begin().await?;
    let mut admin_vpc = db::vpc::find_by_vni(&mut txn, vni as i32).await?;

    let admin_vpc = admin_vpc.remove(0);

    assert_eq!(
        admin_vpc.config.network_virtualization_type,
        VpcVirtualizationType::Fnn
    );

    let admin_segments = db::network_segment::admin(&mut txn).await?;

    for admin_segment in admin_segments {
        assert_eq!(admin_vpc.id, admin_segment.config.vpc_id.unwrap());
    }

    Ok(())
}

#[crate::sqlx_test]
async fn create_admin_vpc_updates_existing_admin_vpc_vni(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let initial_vni = 10000;
    let updated_vni = 10001;

    // Create the initial admin VPC and verify the admin segments attach to it.
    db_init::create_admin_vpc(&env.pool, Some(initial_vni)).await?;
    let mut txn = env.pool.begin().await?;
    let mut initial_admin_vpcs = db::vpc::find_by_vni(&mut txn, initial_vni as i32).await?;
    assert_eq!(initial_admin_vpcs.len(), 1);
    let initial_admin_vpc = initial_admin_vpcs.remove(0);
    for admin_segment in db::network_segment::admin(&mut txn).await? {
        assert_eq!(Some(initial_admin_vpc.id), admin_segment.config.vpc_id);
    }
    txn.commit().await?;

    // Change the configured VNI and run startup reconciliation again.
    db_init::create_admin_vpc(&env.pool, Some(updated_vni)).await?;

    // Fetch from the DB to verify the existing admin VPC was updated in place.
    let mut txn = env.pool.begin().await?;
    let mut updated_admin_vpcs = db::vpc::find_by_vni(&mut txn, updated_vni as i32).await?;
    assert_eq!(updated_admin_vpcs.len(), 1);
    let updated_admin_vpc = updated_admin_vpcs.remove(0);
    assert_eq!(updated_admin_vpc.id, initial_admin_vpc.id);
    assert_eq!(updated_admin_vpc.config.vni, Some(updated_vni as i32));
    assert_eq!(updated_admin_vpc.status.vni, Some(updated_vni as i32));
    assert!(
        db::vpc::find_by_vni(&mut txn, initial_vni as i32)
            .await?
            .is_empty()
    );

    // Verify reconciliation did not create a duplicate admin VPC row.
    let admin_vpcs = db::vpc::find_by_name(&env.pool, "admin").await?;
    assert_eq!(admin_vpcs.len(), 1);

    // Verify every admin segment still points at the same reconciled VPC.
    for admin_segment in db::network_segment::admin(&mut txn).await? {
        assert_eq!(Some(updated_admin_vpc.id), admin_segment.config.vpc_id);
    }

    Ok(())
}

#[crate::sqlx_test]
async fn create_admin_vpc_rejects_existing_tenant_vpc_vni(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let vni = 60001;

    // Create a tenant VPC with the same VNI before the admin VPC is seeded.
    let tenant_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder("tenant-admin-vni-conflict")
                .vni(vni)
                .metadata(rpc::forge::Metadata {
                    name: "tenant-admin-vni-conflict".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner();

    // Verify the VNI actually persisted before running admin reconciliation.
    let mut txn = env.pool.begin().await?;
    let mut tenant_vpcs = db::vpc::find_by_vni(&mut txn, vni as i32).await?;
    assert_eq!(tenant_vpcs.len(), 1);
    assert_eq!(tenant_vpcs.remove(0).id, tenant_vpc.id.unwrap());
    txn.commit().await?;

    // Seeding the admin VPC must fail instead of adopting the tenant VPC.
    let err = db_init::create_admin_vpc(&env.pool, Some(vni))
        .await
        .expect_err("admin VPC seeding should reject an already-used tenant VNI");
    assert!(
        err.to_string()
            .contains("but no admin VPC is attached to admin network segments")
    );

    // Verify the admin segments remain unattached after the rejected seed.
    let mut txn = env.pool.begin().await?;
    for admin_segment in db::network_segment::admin(&mut txn).await? {
        assert!(admin_segment.config.vpc_id.is_none());
    }

    Ok(())
}

#[crate::sqlx_test]
async fn create_update_network_security_group_for_vpc(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    populate_network_security_groups(env.api.clone()).await;

    let good_network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";
    let bad_network_security_group_id = "ddfcabc4-92dc-41e2-874e-2c7eeb9fa156";

    let default_tenant_org = "Tenant1";

    // Attempt to create a VPC with an NSG of a
    // different tenant.  This should fail.
    let _ = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(default_tenant_org)
                .network_security_group_id(bad_network_security_group_id)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Try again with a good NSG ID.
    let vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(default_tenant_org)
                .network_security_group_id(good_network_security_group_id)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Make sure the VPC has the security group we expect

    assert_eq!(
        forge_vpc_config(&vpc).network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    let vpc_id = vpc.id;

    // Attempt to update the VPC with an NSG of a
    // different tenant.  This should fail.
    let _ = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(vpc_id)
                .network_security_group_id(bad_network_security_group_id)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Try again with a good NSG ID.
    let vpc = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(vpc_id)
                .network_security_group_id(good_network_security_group_id)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .vpc
        .unwrap();

    // Make sure the VPC has the security group we expect
    assert_eq!(
        forge_vpc_config(&vpc).network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    // Update again to clear the the NSG attachment.
    let vpc = env
        .api
        .update_vpc(
            VpcUpdateRequest::builder()
                .set_id(vpc_id)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .vpc
        .unwrap();

    // Make sure the VPC has no NSG ID
    assert!(forge_vpc_config(&vpc).network_security_group_id.is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_increment_vpc_version_detects_concurrent_writes(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Two concurrent `increment_vpc_version` calls on the same VPC
    // should not silently lose an increment. Exactly one caller wins
    // (their `WHERE version=$old` matches and updates), and the loser
    // sees 0 rows updated and returns `ConcurrentModificationError`.
    let env = create_test_env(pool.clone()).await;
    let vpc_id: VpcId = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "vpc-bump".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    let initial_version = {
        let vpcs =
            db::vpc::find_by(&pool, ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id)).await?;
        vpcs[0].version
    };
    let initial_version_nr = initial_version.version_nr();

    // Open two transactions, have both use the same expected version, then race!
    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let (a, b) = tokio::join!(
        tokio::spawn(async move {
            let mut txn = pool_a.begin().await.unwrap();
            let result = db::vpc::increment_vpc_version(&mut txn, vpc_id, initial_version).await;
            if result.is_ok() {
                txn.commit().await.unwrap();
            } else {
                txn.rollback().await.unwrap();
            }
            result
        }),
        tokio::spawn(async move {
            let mut txn = pool_b.begin().await.unwrap();
            let result = db::vpc::increment_vpc_version(&mut txn, vpc_id, initial_version).await;
            if result.is_ok() {
                txn.commit().await.unwrap();
            } else {
                txn.rollback().await.unwrap();
            }
            result
        }),
    );
    let (a, b) = (a.unwrap(), b.unwrap());

    let outcomes = [&a, &b];
    let successes = outcomes.iter().filter(|r| r.is_ok()).count();
    let conflicts = outcomes
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(db::DatabaseError::ConcurrentModificationError("vpc", _))
            )
        })
        .count();
    assert_eq!(
        successes, 1,
        "exactly 1 of 2 concurrent increments should succeed; got {successes} (a={a:?}, b={b:?})"
    );
    assert_eq!(
        conflicts, 1,
        "the losing race should get a ConcurrentModificationError; got {conflicts} (a={a:?}, b={b:?})"
    );

    let final_version_nr = {
        let vpcs =
            db::vpc::find_by(&pool, ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id)).await?;
        vpcs[0].version.version_nr()
    };
    assert_eq!(
        final_version_nr - initial_version_nr,
        1,
        "exactly one increment should have happened after the race"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn create_flat_vpc_succeeds_without_routing_profile(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Flat VPCs are for zero-DPU hosts and don't have a Carbide-managed
    // routing layer. The create handler should skip the FNN-flavored
    // routing-profile validation entirely and still allocate a VNI.
    let env = create_test_env(pool).await;

    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "flat-tenant".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "flat-tenant".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await?
        .into_inner()
        .tenant
        .unwrap();

    let vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant.organization_id.clone())
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Flat as i32)
                .metadata(rpc::forge::Metadata {
                    name: "flat".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        forge_vpc_config(&vpc).network_virtualization_type,
        Some(rpc::forge::VpcVirtualizationType::Flat as i32),
    );
    assert!(forge_vpc_config(&vpc).routing_profile_type.is_none());
    assert!(
        vpc.status.as_ref().and_then(|s| s.vni).is_some(),
        "Flat VPCs still allocate a VNI for pluggable SDN hooks (e.g. switch-side VTEPs)",
    );

    Ok(())
}

#[crate::sqlx_test]
async fn create_flat_vpc_rejects_routing_profile_type(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Routing profile types are FNN-specific. Sending one on a Flat VPC
    // create is contradictory and should be rejected up front.
    let env = create_test_env(pool).await;

    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "flat-tenant".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "flat-tenant".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await?
        .into_inner()
        .tenant
        .unwrap();

    let err = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant.organization_id)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Flat as i32)
                .routing_profile_type("EXTERNAL".to_string())
                .metadata(rpc::forge::Metadata {
                    name: "flat".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .expect_err("Flat VPC + routing_profile_type must be rejected");

    assert_eq!(err.code(), tonic::Code::InvalidArgument, "got: {err}");
    assert!(
        err.message().contains("flat") && err.message().contains("routing_profile_type"),
        "error should mention flat VPC and the routing_profile_type field, got: {}",
        err.message()
    );

    Ok(())
}
