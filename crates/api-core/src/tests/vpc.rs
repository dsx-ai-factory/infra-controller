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
use std::time::Duration;

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_test_support::Outcome::FailsWith;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::vpc::VpcId;
use common::api_fixtures::{create_test_env, populate_network_security_groups};
use config_version::ConfigVersion;
use db::vpc::{self};
use db::{self, ObjectColumnFilter};
use model::metadata::Metadata;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use model::vpc::{
    NewVpc, PowerResourceGroupUpdate, UpdateVpc, UpdateVpcVirtualization, VpcDefinition,
    VpcRoutingProfileOverrides, VpcStatus,
};
use rpc::forge::forge_server::Forge;

use crate::test_support::metadata;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common;
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::vpc::create_vpc as create_fixture_vpc;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::{VpcCreationRequest, VpcDeletionRequest, VpcUpdateRequest};
use crate::{DatabaseError, db_init};

type VpcVniPoolState = Vec<(String, String, sqlx::types::Json<ResourcePoolEntryState>)>;

fn forge_vpc_config(vpc: &rpc::forge::Vpc) -> &rpc::forge::VpcConfig {
    vpc.config
        .as_ref()
        .expect("structured config must be populated")
}

async fn find_test_vpc(env: &TestEnv, vpc_id: VpcId) -> Result<rpc::forge::Vpc, tonic::Status> {
    Ok(env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted VPC"))
}

async fn allocate_external_vni(env: &TestEnv, owner_id: VpcId) -> Result<i32, eyre::Report> {
    let mut txn = env.pool.begin().await?;
    let vni = db::resource_pool::allocate(
        &env.common_pools.ethernet.pool_external_vpc_vni,
        &mut txn,
        OwnerType::Vpc,
        &owner_id.to_string(),
        None,
    )
    .await?;
    txn.commit().await?;
    Ok(vni)
}

async fn resource_pool_entry_state(
    env: &TestEnv,
    pool_name: &str,
    vni: i32,
) -> Result<ResourcePoolEntryState, sqlx::Error> {
    let state = sqlx::query_scalar::<_, sqlx::types::Json<ResourcePoolEntryState>>(
        "SELECT state FROM resource_pool WHERE name = $1 AND value = $2",
    )
    .bind(pool_name)
    .bind(vni.to_string())
    .fetch_one(&env.pool)
    .await?;
    Ok(state.0)
}

async fn vpc_vni_pool_state(env: &TestEnv) -> Result<VpcVniPoolState, sqlx::Error> {
    sqlx::query_as(
        "SELECT name, value, state FROM resource_pool
         WHERE name IN ($1, $2) ORDER BY name, value",
    )
    .bind(env.common_pools.ethernet.pool_vpc_vni.name())
    .bind(env.common_pools.ethernet.pool_external_vpc_vni.name())
    .fetch_all(&env.pool)
    .await
}

async fn create_routing_profile_vpc(
    env: &TestEnv,
    name: &str,
    requested_vni: Option<u32>,
) -> Result<rpc::forge::Vpc, tonic::Status> {
    env.api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: name.to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::Metadata {
                name: name.to_string(),
                ..Default::default()
            }),
        }))
        .await?;
    let mut request = VpcCreationRequest::builder(name)
        .metadata(rpc::Metadata {
            name: name.to_string(),
            ..Default::default()
        })
        .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
        .routing_profile_type("INTERNAL".to_string())
        .routing_profile_overrides(rpc::forge::VpcRoutingProfileOverrides::default());
    if let Some(vni) = requested_vni {
        request = request.vni(vni);
    }
    Ok(env
        .api
        .create_vpc(request.tonic_request())
        .await?
        .into_inner())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_retains_allocations_and_creation_intent(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let internal_pool = env.common_pools.ethernet.pool_vpc_vni.name();
    let external_pool = env.common_pools.ethernet.pool_external_vpc_vni.name();

    for (scenario, requested_vni) in [("automatic-intent", None), ("explicit-intent", Some(60001))]
    {
        let created = create_routing_profile_vpc(&env, scenario, requested_vni).await?;
        let vpc_id = created.id.expect("VPC ID");
        let network_security_group_id: carbide_uuid::network_security_group::NetworkSecurityGroupId = scenario.parse()?;
        let mut txn = env.pool.begin().await?;
        db::network_security_group::create(
            &mut txn,
            &network_security_group_id,
            &scenario.parse()?,
            None,
            &Metadata {
                name: scenario.to_string(),
                ..Default::default()
            },
            false,
            &[],
        )
        .await?;
        txn.commit().await?;
        let before = env
            .api
            .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
                id: Some(vpc_id),
                metadata: created.metadata,
                network_security_group_id: Some(network_security_group_id.to_string()),
                ..Default::default()
            }))
            .await?
            .into_inner()
            .vpc
            .expect("updated VPC");
        let internal_vni = before
            .status
            .as_ref()
            .and_then(|status| status.vni)
            .expect("active VNI");
        let destination_vni = if requested_vni.is_some() {
            let (_, value, _) = vpc_vni_pool_state(&env)
                .await?
                .into_iter()
                .find(|(name, _, state)| {
                    name == external_pool && state.0 == ResourcePoolEntryState::Free
                })
                .expect("free automatic external VNI");
            Some(value.parse::<u32>()?)
        } else {
            None
        };
        let forward = env
            .api
            .change_vpc_routing_profile(tonic::Request::new(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(vpc_id),
                    if_version_match: Some(before.version.clone()),
                    routing_profile_type: "EXTERNAL".to_string(),
                    vni: destination_vni,
                },
            ))
            .await?
            .into_inner();
        assert_eq!(forward.id, Some(vpc_id));
        assert_eq!(forward.routing_profile_type.as_deref(), Some("EXTERNAL"));
        assert_ne!(forward.active_vni, internal_vni);
        if let Some(vni) = destination_vni {
            assert_eq!(forward.active_vni, vni);
        }
        assert_eq!(
            forward.retained_allocation,
            Some(rpc::forge::VpcRetainedVniAllocation {
                pool_name: internal_pool.to_string(),
                vni: internal_vni,
            })
        );
        let after = find_test_vpc(&env, vpc_id).await?;
        let mut expected_config = forge_vpc_config(&before).clone();
        expected_config.routing_profile_type = Some("EXTERNAL".to_string());
        assert_eq!(after.config.as_ref(), Some(&expected_config), "{scenario}");
        assert_eq!(after.metadata, before.metadata, "{scenario}");
        assert_eq!(after.version, forward.version);
        assert_eq!(
            after.status.as_ref().and_then(|status| status.vni),
            Some(forward.active_vni)
        );
        assert_eq!(
            forward.version.parse::<ConfigVersion>()?.version_nr(),
            before.version.parse::<ConfigVersion>()?.version_nr() + 1
        );
        for (pool_name, vni) in [
            (internal_pool, internal_vni),
            (external_pool, forward.active_vni),
        ] {
            assert_eq!(
                resource_pool_entry_state(&env, pool_name, i32::try_from(vni)?).await?,
                ResourcePoolEntryState::Allocated {
                    owner: vpc_id.to_string(),
                    owner_type: OwnerType::Vpc.to_string(),
                }
            );
        }

        // The retained VNI is reusable even when no automatic destination
        // remains. Explicit creation also leaves it in a non-auto range.
        if requested_vni.is_some() {
            sqlx::query("UPDATE resource_pool SET auto_assign = false WHERE name = $1")
                .bind(internal_pool)
                .execute(&env.pool)
                .await?;
        }
        let allocations_before_reverse = vpc_vni_pool_state(&env).await?;
        if requested_vni.is_some() {
            let other_vni = internal_vni + 1;
            assert_eq!(
                resource_pool_entry_state(&env, internal_pool, i32::try_from(other_vni)?).await?,
                ResourcePoolEntryState::Free
            );
            let error = env
                .api
                .change_vpc_routing_profile(tonic::Request::new(
                    rpc::forge::VpcChangeRoutingProfileRequest {
                        id: Some(vpc_id),
                        if_version_match: Some(forward.version.clone()),
                        routing_profile_type: "INTERNAL".to_string(),
                        vni: Some(other_vni),
                    },
                ))
                .await
                .expect_err("a free VNI cannot replace a retained allocation");
            assert_eq!(error.code(), tonic::Code::FailedPrecondition);
            assert!(error.message().contains(&other_vni.to_string()), "{error}");
            assert_eq!(find_test_vpc(&env, vpc_id).await?, after);
            assert_eq!(vpc_vni_pool_state(&env).await?, allocations_before_reverse);
        }
        let reverse = env
            .api
            .change_vpc_routing_profile(tonic::Request::new(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(vpc_id),
                    if_version_match: Some(forward.version),
                    routing_profile_type: "INTERNAL".to_string(),
                    vni: requested_vni.map(|_| internal_vni),
                },
            ))
            .await?
            .into_inner();
        assert_eq!(reverse.active_vni, internal_vni);
        assert_eq!(reverse.routing_profile_type.as_deref(), Some("INTERNAL"));
        assert_eq!(
            reverse.retained_allocation,
            Some(rpc::forge::VpcRetainedVniAllocation {
                pool_name: external_pool.to_string(),
                vni: forward.active_vni,
            })
        );
        let restored = find_test_vpc(&env, vpc_id).await?;
        assert_eq!(restored.config, before.config, "{scenario}");
        assert_eq!(restored.status, before.status, "{scenario}");
        assert_eq!(restored.version, reverse.version);
        assert_eq!(vpc_vni_pool_state(&env).await?, allocations_before_reverse);
    }
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_rechecks_tenant_access_for_retained_vni(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let created = create_routing_profile_vpc(&env, "changed-entitlement", None).await?;
    let vpc_id = created.id.expect("VPC ID");
    // Model an operator repair of persisted tenant policy. UpdateTenant
    // rejects changing that policy while an FNN VPC is attached.
    sqlx::query("UPDATE tenants SET routing_profile_type = 'EXTERNAL' WHERE organization_id = $1")
        .bind("changed-entitlement")
        .execute(&env.pool)
        .await?;

    // Repairing an overly broad source remains allowed; only the requested
    // destination must fit the tenant's persisted entitlement.
    let forward = env
        .api
        .change_vpc_routing_profile(tonic::Request::new(
            rpc::forge::VpcChangeRoutingProfileRequest {
                id: Some(vpc_id),
                if_version_match: Some(created.version),
                routing_profile_type: "EXTERNAL".to_string(),
                vni: None,
            },
        ))
        .await?
        .into_inner();
    let before = find_test_vpc(&env, vpc_id).await?;
    let allocations_before = vpc_vni_pool_state(&env).await?;
    let error = env
        .api
        .change_vpc_routing_profile(tonic::Request::new(
            rpc::forge::VpcChangeRoutingProfileRequest {
                id: Some(vpc_id),
                if_version_match: Some(forward.version),
                routing_profile_type: "INTERNAL".to_string(),
                vni: None,
            },
        ))
        .await
        .expect_err("retained ownership does not authorize broader routing");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("broader than associated tenant routing-profile access tier")
    );
    assert_eq!(find_test_vpc(&env, vpc_id).await?, before);
    assert_eq!(vpc_vni_pool_state(&env).await?, allocations_before);
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_rolls_back_allocation_on_write_failure(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let before = create_routing_profile_vpc(&env, "failed-final-write", None).await?;
    let vpc_id = before.id.expect("VPC ID");
    let allocations_before = vpc_vni_pool_state(&env).await?;
    // Fail the VPC write after destination allocation has succeeded.
    sqlx::query("ALTER TABLE vpcs ADD CONSTRAINT reject_profile_change CHECK (routing_profile_type <> 'EXTERNAL') NOT VALID")
        .execute(&env.pool)
        .await?;
    for vni in [None, Some(50001)] {
        let error = env
            .api
            .change_vpc_routing_profile(tonic::Request::new(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(vpc_id),
                    if_version_match: Some(before.version.clone()),
                    routing_profile_type: "EXTERNAL".to_string(),
                    vni,
                },
            ))
            .await
            .expect_err("injected VPC write failure");
        assert!(
            error.message().contains("reject_profile_change"),
            "{vni:?}: {error}"
        );
        assert_eq!(find_test_vpc(&env, vpc_id).await?, before, "{vni:?}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            allocations_before,
            "{vni:?}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_rejects_unavailable_exact_vni_without_changes(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    struct Case<'a> {
        scenario: &'static str,
        vni: u32,
        message_contains: &'a str,
    }

    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let before = create_routing_profile_vpc(&env, "unavailable-exact-vni", None).await?;
    let vpc_id = before.id.expect("VPC ID");
    let active_vni = before
        .status
        .as_ref()
        .and_then(|status| status.vni)
        .expect("active VNI");
    let allocations_before = vpc_vni_pool_state(&env).await?;

    for case in [
        Case {
            scenario: "missing destination value",
            vni: 50000,
            message_contains: env.common_pools.ethernet.pool_external_vpc_vni.name(),
        },
        Case {
            scenario: "already active",
            vni: active_vni,
            message_contains: "already active",
        },
    ] {
        let scenario = case.scenario;
        let error = env
            .api
            .change_vpc_routing_profile(tonic::Request::new(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(vpc_id),
                    if_version_match: Some(before.version.clone()),
                    routing_profile_type: "EXTERNAL".to_string(),
                    vni: Some(case.vni),
                },
            ))
            .await
            .expect_err(scenario);
        assert_eq!(
            error.code(),
            tonic::Code::FailedPrecondition,
            "{scenario}: {error}"
        );
        assert!(
            error.message().contains(&case.vni.to_string()),
            "{scenario}: {error}"
        );
        assert!(
            error.message().contains(case.message_contains),
            "{scenario}: {error}"
        );
        assert_eq!(find_test_vpc(&env, vpc_id).await?, before, "{scenario}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            allocations_before,
            "{scenario}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_rejects_unsupported_state_without_changes(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    #[derive(Clone, Copy, Debug)]
    enum Failure {
        NonFnn,
        SamePolarity,
        MissingSource,
        RemovedSource,
        VpcOverride,
        SeededWrongPool,
        OverlappingPools,
        InvalidDestinationVni,
        ExhaustedDestination,
        MissingDestinationPool,
    }
    struct ExpectedFailure {
        destination: &'static str,
        code: tonic::Code,
        message: &'static str,
    }

    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let created = create_routing_profile_vpc(&env, "unsupported-transition", None).await?;
    let vpc_id = created.id.expect("VPC ID");
    let internal_pool = env.common_pools.ethernet.pool_vpc_vni.name();
    let external_pool = env.common_pools.ethernet.pool_external_vpc_vni.name();
    let original_external_values: Vec<(i32, bool)> = sqlx::query_as(
        "SELECT value::integer, auto_assign FROM resource_pool WHERE name = $1 ORDER BY value",
    )
    .bind(external_pool)
    .fetch_all(&env.pool)
    .await?;
    let original_allocations = vpc_vni_pool_state(&env).await?;

    for failure in [
        Failure::NonFnn,
        Failure::SamePolarity,
        Failure::MissingSource,
        Failure::RemovedSource,
        Failure::VpcOverride,
        Failure::SeededWrongPool,
        Failure::OverlappingPools,
        Failure::InvalidDestinationVni,
        Failure::ExhaustedDestination,
        Failure::MissingDestinationPool,
    ] {
        sqlx::query("UPDATE vpcs SET routing_profile_type = 'INTERNAL', routing_profile_overrides = $1, network_virtualization_type = $2 WHERE id = $3")
            .bind(sqlx::types::Json(VpcRoutingProfileOverrides::default()))
            .bind(VpcVirtualizationType::Fnn)
            .bind(vpc_id)
            .execute(&env.pool)
            .await?;
        let expected = match failure {
            Failure::NonFnn => {
                sqlx::query("UPDATE vpcs SET network_virtualization_type = $1 WHERE id = $2")
                    .bind(VpcVirtualizationType::EthernetVirtualizer)
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "routing-profile changes require an FNN VPC",
                }
            }
            Failure::SamePolarity => ExpectedFailure {
                destination: "INTERNAL",
                code: tonic::Code::FailedPrecondition,
                message: "opposite internal settings",
            },
            Failure::MissingSource => {
                sqlx::query("UPDATE vpcs SET routing_profile_type = NULL WHERE id = $1")
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "named source",
                }
            }
            Failure::RemovedSource => {
                sqlx::query("UPDATE vpcs SET routing_profile_type = 'REMOVED' WHERE id = $1")
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::NotFound,
                    message: "REMOVED",
                }
            }
            Failure::VpcOverride => {
                sqlx::query("UPDATE vpcs SET routing_profile_overrides = $1 WHERE id = $2")
                    .bind(sqlx::types::Json(VpcRoutingProfileOverrides {
                        leak_default_route_from_underlay: Some(true),
                        ..Default::default()
                    }))
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "VPC routing overrides",
                }
            }
            Failure::SeededWrongPool => {
                sqlx::query("UPDATE vpcs SET routing_profile_type = 'EXTERNAL' WHERE id = $1")
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "INTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "already owns the active VNI",
                }
            }
            Failure::OverlappingPools => {
                sqlx::query("INSERT INTO resource_pool (name, value, state, auto_assign, value_type) SELECT $1, value, state, auto_assign, value_type FROM resource_pool WHERE name = $2 AND value = $3")
                    .bind(external_pool).bind(internal_pool).bind("60001")
                    .execute(&env.pool).await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "pools overlap at VNI `60001`",
                }
            }
            Failure::InvalidDestinationVni => {
                // Materialized pools can contain a value outside the wire VNI
                // domain. Make it the sole automatic choice to prove rollback.
                sqlx::query("UPDATE resource_pool SET value = '16777216' WHERE name = $1 AND value = '50001'")
                    .bind(external_pool).execute(&env.pool).await?;
                sqlx::query(
                    "UPDATE resource_pool SET auto_assign = (value = '16777216') WHERE name = $1",
                )
                .bind(external_pool)
                .execute(&env.pool)
                .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "between 1 and 16777215",
                }
            }
            Failure::ExhaustedDestination => {
                sqlx::query("UPDATE resource_pool SET auto_assign = false WHERE name = $1")
                    .bind(external_pool)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::ResourceExhausted,
                    message: "external-vpc-vni",
                }
            }
            Failure::MissingDestinationPool => {
                sqlx::query("DELETE FROM resource_pool WHERE name = $1")
                    .bind(external_pool)
                    .execute(&env.pool)
                    .await?;
                ExpectedFailure {
                    destination: "EXTERNAL",
                    code: tonic::Code::FailedPrecondition,
                    message: "no materialized values",
                }
            }
        };
        let before = find_test_vpc(&env, vpc_id).await?;
        let allocations_before = vpc_vni_pool_state(&env).await?;
        let error = env
            .api
            .change_vpc_routing_profile(tonic::Request::new(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(vpc_id),
                    if_version_match: Some(before.version.clone()),
                    routing_profile_type: expected.destination.to_string(),
                    vni: None,
                },
            ))
            .await
            .expect_err(expected.message);
        assert_eq!(error.code(), expected.code, "{failure:?}: {error}");
        assert!(
            error.message().contains(expected.message),
            "{failure:?}: {error}"
        );
        assert_eq!(find_test_vpc(&env, vpc_id).await?, before, "{failure:?}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            allocations_before,
            "{failure:?}"
        );

        // Every row starts with the same free destination values and automatic
        // assignment flags, even after pool exhaustion or removal.
        let mut txn = env.pool.begin().await?;
        sqlx::query("DELETE FROM resource_pool WHERE name = $1")
            .bind(external_pool)
            .execute(&mut *txn)
            .await?;
        for auto_assign in [false, true] {
            let values = original_external_values
                .iter()
                .filter(|(_, original_auto_assign)| *original_auto_assign == auto_assign)
                .map(|(value, _)| *value)
                .collect();
            db::resource_pool::populate(
                &env.common_pools.ethernet.pool_external_vpc_vni,
                &mut txn,
                values,
                auto_assign,
            )
            .await?;
        }
        txn.commit().await?;
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            original_allocations,
            "{failure:?}"
        );
        let restored_external_values: Vec<(i32, bool)> = sqlx::query_as(
            "SELECT value::integer, auto_assign FROM resource_pool WHERE name = $1 ORDER BY value",
        )
        .bind(external_pool)
        .fetch_all(&env.pool)
        .await?;
        assert_eq!(
            restored_external_values, original_external_values,
            "{failure:?}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_same_version_commits_once(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let before = create_routing_profile_vpc(&env, "concurrent-profile-change", None).await?;
    let vpc_id = before.id.expect("VPC ID");
    let request = rpc::forge::VpcChangeRoutingProfileRequest {
        id: Some(vpc_id),
        if_version_match: Some(before.version.clone()),
        routing_profile_type: "EXTERNAL".to_string(),
        vni: None,
    };
    let (first, second) = tokio::join!(
        env.api
            .change_vpc_routing_profile(tonic::Request::new(request.clone())),
        env.api
            .change_vpc_routing_profile(tonic::Request::new(request)),
    );
    let (changed, rejected) = match (first, second) {
        (Ok(changed), Err(rejected)) | (Err(rejected), Ok(changed)) => {
            (changed.into_inner(), rejected)
        }
        other => panic!("exactly one mutation must commit: {other:?}"),
    };
    assert_eq!(rejected.code(), tonic::Code::FailedPrecondition);
    assert!(rejected.message().contains(&before.version));
    assert_eq!(
        changed.version.parse::<ConfigVersion>()?.version_nr(),
        before.version.parse::<ConfigVersion>()?.version_nr() + 1
    );
    assert_eq!(find_test_vpc(&env, vpc_id).await?.version, changed.version);
    let allocations = vpc_vni_pool_state(&env).await?;
    assert_eq!(
        allocations
            .iter()
            .filter(|(_, _, state)| matches!(&state.0,
                ResourcePoolEntryState::Allocated { owner, owner_type }
                if owner == &vpc_id.to_string() && owner_type == &OwnerType::Vpc.to_string()
            ))
            .count(),
        2
    );
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_exact_vni_has_one_owner_under_concurrent_requests(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let before = [
        create_routing_profile_vpc(&env, "first-exact-vni-request", None).await?,
        create_routing_profile_vpc(&env, "second-exact-vni-request", None).await?,
    ];
    let external_pool = env.common_pools.ethernet.pool_external_vpc_vni.name();
    let mut expected_allocations = vpc_vni_pool_state(&env).await?;
    let requested_vni = 50001;
    let requests = before
        .each_ref()
        .map(|vpc| rpc::forge::VpcChangeRoutingProfileRequest {
            id: vpc.id,
            if_version_match: Some(vpc.version.clone()),
            routing_profile_type: "EXTERNAL".to_string(),
            vni: Some(requested_vni),
        });
    let (first, second) = tokio::join!(
        env.api
            .change_vpc_routing_profile(tonic::Request::new(requests[0].clone())),
        env.api
            .change_vpc_routing_profile(tonic::Request::new(requests[1].clone())),
    );
    let (winner, changed, rejected) = match (first, second) {
        (Ok(changed), Err(rejected)) => (0, changed.into_inner(), rejected),
        (Err(rejected), Ok(changed)) => (1, changed.into_inner(), rejected),
        other => panic!("exactly one VPC must claim the requested VNI: {other:?}"),
    };
    let winner_before = &before[winner];
    let winner_id = winner_before.id.expect("winning VPC ID");
    let loser_before = &before[1 - winner];
    assert_eq!(rejected.code(), tonic::Code::FailedPrecondition);
    assert!(
        rejected.message().contains(&requested_vni.to_string()),
        "{rejected}"
    );
    assert!(rejected.message().contains(external_pool), "{rejected}");
    assert_eq!(changed.id, Some(winner_id));
    assert_eq!(changed.routing_profile_type.as_deref(), Some("EXTERNAL"));
    assert_eq!(changed.active_vni, requested_vni);
    assert_eq!(
        find_test_vpc(&env, loser_before.id.expect("losing VPC ID")).await?,
        *loser_before
    );
    let (_, _, state) = expected_allocations
        .iter_mut()
        .find(|(name, value, _)| name == external_pool && value == &requested_vni.to_string())
        .expect("requested external VNI");
    assert_eq!(state.0, ResourcePoolEntryState::Free);
    *state = sqlx::types::Json(ResourcePoolEntryState::Allocated {
        owner: winner_id.to_string(),
        owner_type: OwnerType::Vpc.to_string(),
    });
    let allocations_after = vpc_vni_pool_state(&env).await?;
    assert_eq!(allocations_after, expected_allocations);
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_rejects_site_global_vni(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let mut config = crate::tests::common::api_fixtures::get_config();
    config.site_global_vpc_vni = Some(10000);
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_fnn_config(None),
    )
    .await;
    let before = create_routing_profile_vpc(&env, "site-global-vni", None).await?;
    let vpc_id = before.id.expect("VPC ID");
    let allocations = vpc_vni_pool_state(&env).await?;
    let error = env
        .api
        .change_vpc_routing_profile(tonic::Request::new(
            rpc::forge::VpcChangeRoutingProfileRequest {
                id: Some(vpc_id),
                if_version_match: Some(before.version.clone()),
                routing_profile_type: "EXTERNAL".to_string(),
                vni: None,
            },
        ))
        .await
        .expect_err("per-VPC status does not control a site-global VNI");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("routing-profile changes do not support a site-global VNI")
    );
    assert_eq!(find_test_vpc(&env, vpc_id).await?, before);
    assert_eq!(vpc_vni_pool_state(&env).await?, allocations);
    Ok(())
}

#[crate::sqlx_test]
async fn change_vpc_routing_profile_requires_fnn_configuration(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let (vpc_id, _) = create_fixture_vpc(&env, "FNN disabled".to_string(), None, None).await;
    // Persisted FNN VPCs can survive removal of the site's FNN config.
    sqlx::query("UPDATE vpcs SET network_virtualization_type = $1 WHERE id = $2")
        .bind(VpcVirtualizationType::Fnn)
        .bind(vpc_id)
        .execute(&env.pool)
        .await?;
    let before = find_test_vpc(&env, vpc_id).await?;
    let allocations = vpc_vni_pool_state(&env).await?;
    let error = env
        .api
        .change_vpc_routing_profile(tonic::Request::new(
            rpc::forge::VpcChangeRoutingProfileRequest {
                id: Some(vpc_id),
                if_version_match: Some(before.version.clone()),
                routing_profile_type: "EXTERNAL".to_string(),
                vni: None,
            },
        ))
        .await
        .expect_err("FNN config is required for persisted FNN VPCs");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("FNN configuration is required"));
    assert_eq!(find_test_vpc(&env, vpc_id).await?, before);
    assert_eq!(vpc_vni_pool_state(&env).await?, allocations);
    Ok(())
}

/// Verifies an active non-FNN VPC does not prevent repairing a historical profileless tenant, so
/// existing non-FNN workloads do not block later FNN adoption.
#[crate::sqlx_test]
async fn create_vpc_for_tenant_without_profile(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let tenant_organization_id = "historical-profileless-tenant";

    // Persist the historical state produced by creating a tenant before enabling FNN.
    let mut txn = env.pool.begin().await?;
    db::tenant::create_and_persist(
        tenant_organization_id.to_string(),
        Metadata {
            name: "Historical profileless tenant".to_string(),
            ..Default::default()
        },
        None,
        txn.as_mut(),
    )
    .await?;
    txn.commit().await?;

    // Omitting the VPC profile must not persist an FNN VPC without named routing policy.
    let error = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "Profileless FNN VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .expect_err("an FNN VPC requires a tenant routing profile");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("must have a routing profile"));

    // Read through the public API to prove the rejected request persisted no VPC.
    let persisted_ids = env
        .api
        .find_vpc_ids(tonic::Request::new(rpc::forge::VpcSearchFilter {
            name: None,
            tenant_org_id: Some(tenant_organization_id.to_string()),
            label: None,
        }))
        .await?
        .into_inner()
        .vpc_ids;
    assert!(persisted_ids.is_empty());

    // Create an ETV VPC to prove non-FNN workloads do not prevent profile remediation.
    env.api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "Historical tenant ETV VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(
                    rpc::forge::VpcVirtualizationType::EthernetVirtualizer as i32,
                )
                .tonic_request(),
        )
        .await?;

    // Load the tenant through the public API so the update uses its persisted version and metadata.
    let tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: tenant_organization_id.to_string(),
        }))
        .await?
        .into_inner()
        .tenant
        .expect("historical tenant");

    // Assign a valid profile while only the non-FNN VPC is active.
    let updated_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: tenant_organization_id.to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: tenant.metadata,
            if_version_match: Some(tenant.version),
        }))
        .await?
        .into_inner()
        .tenant
        .expect("updated tenant");
    assert_eq!(
        updated_tenant.routing_profile_type.as_deref(),
        Some("INTERNAL")
    );

    // Reload through the public API to prove the profile and new version were persisted.
    let persisted_tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: tenant_organization_id.to_string(),
        }))
        .await?
        .into_inner()
        .tenant
        .expect("persisted tenant");
    assert_eq!(
        persisted_tenant.routing_profile_type.as_deref(),
        Some("INTERNAL")
    );
    assert_eq!(persisted_tenant.version, updated_tenant.version);

    // A later FNN VPC can now inherit the tenant's repaired named profile.
    let fnn_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant_organization_id)
                .metadata(rpc::forge::Metadata {
                    name: "Repaired tenant FNN VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let created_config = forge_vpc_config(&fnn_vpc);
    assert_eq!(
        created_config.network_virtualization_type,
        Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
    );
    assert_eq!(
        created_config.routing_profile_type.as_deref(),
        Some("INTERNAL")
    );

    // Reload the FNN VPC to prove the inherited profile was persisted.
    let persisted_vpc = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![fnn_vpc.id.expect("created FNN VPC ID")],
        }))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .expect("persisted FNN VPC");
    let persisted_config = forge_vpc_config(&persisted_vpc);
    assert_eq!(
        persisted_config.network_virtualization_type,
        Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
    );
    assert_eq!(
        persisted_config.routing_profile_type.as_deref(),
        Some("INTERNAL")
    );

    Ok(())
}

/// Verifies only FNN VPC creation requires a persisted tenant because its routing policy depends
/// on tenant context, while non-FNN creation retains the legacy missing-tenant behavior.
#[crate::sqlx_test]
async fn create_fnn_vpc_requires_existing_tenant(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let missing_tenant = "missing-vpc-tenant";

    // An FNN VPC cannot resolve inherited routing policy without a tenant record.
    let error = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(missing_tenant)
                .metadata(Metadata {
                    name: "FNN without tenant".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .expect_err("an FNN VPC without a tenant must fail");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("must exist before creating an FNN VPC")
    );

    // A non-FNN VPC does not consume tenant routing policy, so preserve its existing behavior.
    let etv_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(missing_tenant)
                .metadata(Metadata {
                    name: "ETV without tenant".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(
                    rpc::forge::VpcVirtualizationType::EthernetVirtualizer as i32,
                )
                .tonic_request(),
        )
        .await?
        .into_inner();

    // Reload through the public API to prove the rejected FNN request created no additional VPC.
    let persisted_ids = env
        .api
        .find_vpc_ids(tonic::Request::new(rpc::forge::VpcSearchFilter {
            name: None,
            tenant_org_id: Some(missing_tenant.to_string()),
            label: None,
        }))
        .await?
        .into_inner()
        .vpc_ids;
    assert_eq!(persisted_ids, vec![etv_vpc.id.expect("created ETV VPC ID")]);

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

    // Model historical invalid rows and configuration drift directly because the public FNN
    // creation path now requires a tenant and runtime configuration is immutable after startup.
    let profileless_id = VpcId::new();
    let stale_profile_id = VpcId::new();
    let mut txn = env.pool.begin().await?;
    db::vpc::persist(
        NewVpc {
            id: profileless_id,
            tenant_organization_id: "profileless-vpc".to_string(),
            network_virtualization_type: VpcVirtualizationType::Fnn,
            metadata: Metadata {
                name: "profileless VPC".to_string(),
                ..Default::default()
            },
            network_security_group_id: None,
            routing_profile_type: None,
            routing_profile_overrides: None,
            power_resource_group: None,
            vni: None,
            slaac_enabled: false,
        },
        VpcStatus { vni: None },
        &mut txn,
    )
    .await?;
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
async fn get_vpc_routing_state_reports_persisted_allocations_without_changes(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    enum AllocationLayout {
        InternalOnly,
        RetainedExternal,
        RetainedInternal,
    }
    struct TestCase {
        scenario: &'static str,
        virtualization: VpcVirtualizationType,
        profile: Option<&'static str>,
        layout: AllocationLayout,
        internal_vni: i32,
    }

    let env = create_test_env(pool).await;
    assert!(env.config.fnn.is_none());
    for TestCase {
        scenario,
        virtualization,
        profile,
        layout,
        internal_vni,
    } in [
        TestCase {
            scenario: "profileless ETV with an owned active VNI of zero",
            virtualization: VpcVirtualizationType::EthernetVirtualizer,
            profile: None,
            layout: AllocationLayout::InternalOnly,
            internal_vni: 0,
        },
        TestCase {
            scenario: "FNN with an active VNI beyond 24 bits and retained external allocation",
            virtualization: VpcVirtualizationType::Fnn,
            profile: Some("REMOVED_PROFILE"),
            layout: AllocationLayout::RetainedExternal,
            internal_vni: 16_777_216,
        },
        TestCase {
            scenario: "Flat with a retained internal allocation",
            virtualization: VpcVirtualizationType::Flat,
            profile: None,
            layout: AllocationLayout::RetainedInternal,
            internal_vni: 20_000,
        },
    ] {
        let mut txn = env.pool.begin().await?;
        sqlx::query("UPDATE resource_pool SET auto_assign = false WHERE name = $1")
            .bind(env.common_pools.ethernet.pool_vpc_vni.name())
            .execute(txn.as_mut())
            .await?;
        db::resource_pool::populate(
            &env.common_pools.ethernet.pool_vpc_vni,
            txn.as_mut(),
            vec![internal_vni],
            true,
        )
        .await?;
        txn.commit().await?;
        let (vpc_id, created) = create_fixture_vpc(&env, scenario.to_string(), None, None).await;
        let internal_vni = u32::try_from(internal_vni)?;
        assert_eq!(
            created.status.as_ref().and_then(|status| status.vni),
            Some(internal_vni),
            "{scenario}"
        );
        let (active_vni, retained) = match layout {
            AllocationLayout::InternalOnly => (internal_vni, None),
            AllocationLayout::RetainedExternal => (
                internal_vni,
                Some((
                    env.common_pools.ethernet.pool_external_vpc_vni.name(),
                    u32::try_from(allocate_external_vni(&env, vpc_id).await?)?,
                )),
            ),
            AllocationLayout::RetainedInternal => (
                u32::try_from(allocate_external_vni(&env, vpc_id).await?)?,
                Some((env.common_pools.ethernet.pool_vpc_vni.name(), internal_vni)),
            ),
        };
        sqlx::query(
            "UPDATE vpcs SET network_virtualization_type = $1, routing_profile_type = $2,
             status = $3 WHERE id = $4",
        )
        .bind(virtualization)
        .bind(profile)
        .bind(sqlx::types::Json(VpcStatus {
            vni: Some(i32::try_from(active_vni)?),
        }))
        .bind(vpc_id)
        .execute(&env.pool)
        .await?;
        let before =
            db::vpc::find_by(&env.pool, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id)).await?;
        let pool_state_before = vpc_vni_pool_state(&env).await?;
        let state = env
            .api
            .get_vpc_routing_state(tonic::Request::new(rpc::forge::VpcRoutingStateRequest {
                id: Some(vpc_id),
            }))
            .await?
            .into_inner();
        assert_eq!(
            state,
            rpc::forge::VpcRoutingState {
                id: Some(vpc_id),
                version: created.version,
                routing_profile_type: profile.map(str::to_string),
                active_vni,
                retained_allocation: retained.map(|(pool_name, vni)| {
                    rpc::forge::VpcRetainedVniAllocation {
                        pool_name: pool_name.to_string(),
                        vni,
                    }
                }),
            },
            "{scenario}"
        );
        assert_eq!(
            db::vpc::find_by(&env.pool, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id)).await?,
            before,
            "{scenario}"
        );
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            pool_state_before,
            "{scenario}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn get_vpc_routing_state_distinguishes_absence_from_invalid_allocations(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    enum InvalidState {
        MissingStatus,
        WrongOwnerType,
        DuplicateRetained,
        ActiveMismatch,
        RetainedValue(&'static str),
    }
    struct TestCase {
        scenario: &'static str,
        invalid_state: InvalidState,
        expected_code: tonic::Code,
    }

    let env = create_test_env(pool).await;
    for (id, expected_code) in [
        (None, tonic::Code::InvalidArgument),
        (Some(VpcId::new()), tonic::Code::NotFound),
    ] {
        let error = env
            .api
            .get_vpc_routing_state(tonic::Request::new(rpc::forge::VpcRoutingStateRequest {
                id,
            }))
            .await
            .expect_err("missing identity must fail");
        assert_eq!(error.code(), expected_code);
    }

    for TestCase {
        scenario,
        invalid_state,
        expected_code,
    } in [
        TestCase {
            scenario: "missing active status",
            invalid_state: InvalidState::MissingStatus,
            expected_code: tonic::Code::FailedPrecondition,
        },
        TestCase {
            scenario: "same owner ID with another owner type",
            invalid_state: InvalidState::WrongOwnerType,
            expected_code: tonic::Code::FailedPrecondition,
        },
        TestCase {
            scenario: "duplicate retained allocations",
            invalid_state: InvalidState::DuplicateRetained,
            expected_code: tonic::Code::FailedPrecondition,
        },
        TestCase {
            scenario: "active status matches neither allocation",
            invalid_state: InvalidState::ActiveMismatch,
            expected_code: tonic::Code::FailedPrecondition,
        },
        TestCase {
            scenario: "negative retained VNI",
            invalid_state: InvalidState::RetainedValue("-1"),
            expected_code: tonic::Code::FailedPrecondition,
        },
        TestCase {
            scenario: "noninteger retained VNI",
            invalid_state: InvalidState::RetainedValue("not-a-vni"),
            expected_code: tonic::Code::Internal,
        },
    ] {
        let (vpc_id, created) = create_fixture_vpc(&env, scenario.to_string(), None, None).await;
        let active_vni = created
            .status
            .as_ref()
            .and_then(|status| status.vni)
            .unwrap();
        let retained_vni = allocate_external_vni(&env, vpc_id).await?;
        match invalid_state {
            InvalidState::MissingStatus | InvalidState::ActiveMismatch => {
                let vni = match invalid_state {
                    InvalidState::MissingStatus => None,
                    _ => Some(retained_vni + 1),
                };
                assert_ne!(vni, Some(i32::try_from(active_vni)?));
                sqlx::query("UPDATE vpcs SET status = $1 WHERE id = $2")
                    .bind(sqlx::types::Json(VpcStatus { vni }))
                    .bind(vpc_id)
                    .execute(&env.pool)
                    .await?;
            }
            InvalidState::WrongOwnerType => {
                sqlx::query("UPDATE resource_pool SET state = $1 WHERE name = $2 AND value = $3")
                    .bind(sqlx::types::Json(ResourcePoolEntryState::Allocated {
                        owner: vpc_id.to_string(),
                        owner_type: OwnerType::Machine.to_string(),
                    }))
                    .bind(env.common_pools.ethernet.pool_vpc_vni.name())
                    .bind(active_vni.to_string())
                    .execute(&env.pool)
                    .await?;
            }
            InvalidState::DuplicateRetained => {
                allocate_external_vni(&env, vpc_id).await?;
            }
            InvalidState::RetainedValue(value) => {
                sqlx::query("UPDATE resource_pool SET value = $1 WHERE name = $2 AND value = $3")
                    .bind(value)
                    .bind(env.common_pools.ethernet.pool_external_vpc_vni.name())
                    .bind(retained_vni.to_string())
                    .execute(&env.pool)
                    .await?;
            }
        }
        let before =
            db::vpc::find_by(&env.pool, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id)).await?;
        let pool_state_before = vpc_vni_pool_state(&env).await?;
        let error = env
            .api
            .get_vpc_routing_state(tonic::Request::new(rpc::forge::VpcRoutingStateRequest {
                id: Some(vpc_id),
            }))
            .await
            .expect_err(scenario);
        assert_eq!(error.code(), expected_code, "{scenario}: {error}");
        assert_eq!(
            db::vpc::find_by(&env.pool, ObjectColumnFilter::One(vpc::IdColumn, &vpc_id)).await?,
            before,
            "{scenario}"
        );
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            pool_state_before,
            "{scenario}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn get_vpc_routing_state_holds_vpc_lock_until_allocations_are_read(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let (vpc_id, created) =
        create_fixture_vpc(&env, "routing state race".to_string(), None, None).await;
    let active_vni = created
        .status
        .as_ref()
        .and_then(|status| status.vni)
        .unwrap();
    let retained_vni = u32::try_from(allocate_external_vni(&env, vpc_id).await?)?;
    let mut allocation_lock = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(allocation_lock.as_mut())
        .await?;
    assert_eq!(
        db::resource_pool::find_owned_allocation(
            &env.common_pools.ethernet.pool_vpc_vni,
            allocation_lock.as_mut(),
            OwnerType::Vpc,
            &vpc_id.to_string(),
        )
        .await?,
        Some(i32::try_from(active_vni)?)
    );

    let read =
        env.api
            .get_vpc_routing_state(tonic::Request::new(rpc::forge::VpcRoutingStateRequest {
                id: Some(vpc_id),
            }));
    tokio::pin!(read);
    let reader_pid = tokio::select! {
        result = &mut read => panic!("read passed a locked allocation: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "resource_pool") => pid,
    };
    let cleanup = env.api.release_vpc_inactive_vni(tonic::Request::new(
        rpc::forge::VpcReleaseInactiveVniRequest {
            id: Some(vpc_id),
            if_version_match: Some(created.version.clone()),
            expected_inactive_vni: Some(retained_vni),
        },
    ));
    tokio::pin!(cleanup);
    tokio::select! {
        result = &mut read => panic!("read passed a locked allocation: {result:?}"),
        result = &mut cleanup => panic!("cleanup passed the reader's VPC lock: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, reader_pid, "vpcs") => {}
    }
    allocation_lock.commit().await?;
    let (read, cleanup) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::try_join!(read, cleanup)
    })
    .await??;
    let original_state = rpc::forge::VpcRoutingState {
        id: Some(vpc_id),
        version: created.version.clone(),
        routing_profile_type: forge_vpc_config(&created).routing_profile_type.clone(),
        active_vni,
        retained_allocation: Some(rpc::forge::VpcRetainedVniAllocation {
            pool_name: env
                .common_pools
                .ethernet
                .pool_external_vpc_vni
                .name()
                .to_string(),
            vni: retained_vni,
        }),
    };
    assert_eq!(read.into_inner(), original_state);
    let cleanup = cleanup.into_inner();
    assert_eq!(cleanup.released_inactive_vni, retained_vni);
    let updated = cleanup.vpc.expect("cleanup returns updated VPC");
    assert_ne!(updated.version, created.version);
    let current_state = env
        .api
        .get_vpc_routing_state(tonic::Request::new(rpc::forge::VpcRoutingStateRequest {
            id: Some(vpc_id),
        }))
        .await?
        .into_inner();
    assert_eq!(
        current_state,
        rpc::forge::VpcRoutingState {
            version: updated.version,
            retained_allocation: None,
            ..original_state
        }
    );
    Ok(())
}

#[crate::sqlx_test]
async fn release_vpc_inactive_vni_preserves_active_state_and_rejects_replay(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    #[derive(Clone, Copy)]
    enum ActivePool {
        Internal,
        External,
    }

    let env = create_test_env(pool).await;
    populate_network_security_groups(env.api.clone()).await;
    let network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    for (scenario, active_pool) in [
        ("release retained external VNI", ActivePool::Internal),
        ("release retained internal VNI", ActivePool::External),
    ] {
        let (vpc_id, mut created) =
            create_fixture_vpc(&env, scenario.to_string(), None, None).await;

        // Check configuration preservation with an attached NSG as well as
        // without one; the full config comparison below includes the NSG ID.
        if matches!(active_pool, ActivePool::Internal) {
            created = env
                .api
                .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
                    id: Some(vpc_id),
                    metadata: created.metadata.clone(),
                    network_security_group_id: Some(network_security_group_id.to_string()),
                    ..Default::default()
                }))
                .await?
                .into_inner()
                .vpc
                .expect("updated VPC");
        }

        let internal_vni = i32::try_from(
            created
                .status
                .as_ref()
                .and_then(|status| status.vni)
                .expect("created VPC has an active VNI"),
        )?;
        let external_vni = allocate_external_vni(&env, vpc_id).await?;

        if matches!(active_pool, ActivePool::External) {
            // Model a completed internal-to-external transition that retained
            // the previous internal allocation for rollback.
            let mut txn = env.pool.begin().await?;
            let vpc = db::vpc::find_by(
                txn.as_mut(),
                ObjectColumnFilter::One(vpc::IdColumn, &vpc_id),
            )
            .await?
            .pop()
            .expect("persisted VPC");
            db::vpc::set_vni(&vpc, &mut txn, external_vni).await?;
            txn.commit().await?;
        }
        let current = find_test_vpc(&env, vpc_id).await?;

        let (active_vni, inactive_vni, active_pool_name, inactive_pool_name) = match active_pool {
            ActivePool::Internal => (
                internal_vni,
                external_vni,
                env.common_pools.ethernet.pool_vpc_vni.name(),
                env.common_pools.ethernet.pool_external_vpc_vni.name(),
            ),
            ActivePool::External => (
                external_vni,
                internal_vni,
                env.common_pools.ethernet.pool_external_vpc_vni.name(),
                env.common_pools.ethernet.pool_vpc_vni.name(),
            ),
        };

        let initial_version: ConfigVersion = current.version.parse()?;
        let request = rpc::forge::VpcReleaseInactiveVniRequest {
            id: Some(vpc_id),
            if_version_match: Some(current.version.clone()),
            expected_inactive_vni: Some(u32::try_from(inactive_vni)?),
        };
        let result = env
            .api
            .release_vpc_inactive_vni(tonic::Request::new(request.clone()))
            .await?
            .into_inner();
        assert_eq!(
            result.released_inactive_vni,
            u32::try_from(inactive_vni)?,
            "{scenario}"
        );

        let updated = result.vpc.expect("updated VPC");
        let updated_version: ConfigVersion = updated.version.parse()?;
        assert_eq!(
            updated_version.version_nr(),
            initial_version.version_nr() + 1,
            "{scenario}"
        );
        assert_eq!(updated.metadata, current.metadata, "{scenario}");
        assert_eq!(updated.config, current.config, "{scenario}");
        assert_eq!(updated.status, current.status, "{scenario}");
        assert_eq!(find_test_vpc(&env, vpc_id).await?, updated, "{scenario}");

        assert_eq!(
            resource_pool_entry_state(&env, active_pool_name, active_vni).await?,
            ResourcePoolEntryState::Allocated {
                owner: vpc_id.to_string(),
                owner_type: OwnerType::Vpc.to_string(),
            },
            "{scenario}"
        );
        assert_eq!(
            resource_pool_entry_state(&env, inactive_pool_name, inactive_vni).await?,
            ResourcePoolEntryState::Free,
            "{scenario}"
        );

        let pool_state_after_release = vpc_vni_pool_state(&env).await?;
        let error = env
            .api
            .release_vpc_inactive_vni(tonic::Request::new(request))
            .await
            .expect_err("replaying the original cleanup request must fail stale");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert!(
            error.message().contains(&format!(
                "did not have the expected version {}",
                current.version
            )),
            "{scenario}: {error}"
        );
        assert_eq!(find_test_vpc(&env, vpc_id).await?, updated, "{scenario}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            pool_state_after_release,
            "{scenario}"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn release_vpc_inactive_vni_failures_are_atomic(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    #[derive(Clone, Copy)]
    enum CleanupFailure {
        MissingVersion,
        MissingExpectedVni,
        InvalidExpectedVni(u32),
        UnexpectedInactiveVni,
        StaleVersion,
        NoInactiveVni,
        ActiveAllocationMismatch,
        DuplicateInactiveVni,
    }

    let env = create_test_env(pool).await;
    let cases = [
        ("missing current version", CleanupFailure::MissingVersion),
        (
            "missing expected inactive VNI",
            CleanupFailure::MissingExpectedVni,
        ),
        (
            "zero expected inactive VNI",
            CleanupFailure::InvalidExpectedVni(0),
        ),
        (
            "expected inactive VNI exceeds 24 bits",
            CleanupFailure::InvalidExpectedVni(16_777_216),
        ),
        (
            "expected inactive VNI does not match",
            CleanupFailure::UnexpectedInactiveVni,
        ),
        ("stale current version", CleanupFailure::StaleVersion),
        ("no inactive allocation", CleanupFailure::NoInactiveVni),
        (
            "active allocation owned by another VPC",
            CleanupFailure::ActiveAllocationMismatch,
        ),
        (
            "duplicate inactive allocations",
            CleanupFailure::DuplicateInactiveVni,
        ),
    ];

    for (scenario, failure) in cases {
        let (vpc_id, created) = create_fixture_vpc(&env, scenario.to_string(), None, None).await;
        let active_vni = created
            .status
            .as_ref()
            .and_then(|status| status.vni)
            .expect("created VPC has an active VNI");

        let inactive_vni = if matches!(failure, CleanupFailure::NoInactiveVni) {
            None
        } else {
            Some(u32::try_from(allocate_external_vni(&env, vpc_id).await?)?)
        };
        if matches!(failure, CleanupFailure::DuplicateInactiveVni) {
            allocate_external_vni(&env, vpc_id).await?;
        }

        if matches!(failure, CleanupFailure::ActiveAllocationMismatch) {
            let foreign_owner_state = ResourcePoolEntryState::Allocated {
                owner: VpcId::new().to_string(),
                owner_type: OwnerType::Vpc.to_string(),
            };
            sqlx::query(
                "UPDATE resource_pool SET state = $1
                 WHERE name = $2 AND value = $3",
            )
            .bind(sqlx::types::Json(foreign_owner_state))
            .bind(env.common_pools.ethernet.pool_vpc_vni.name())
            .bind(active_vni.to_string())
            .execute(&env.pool)
            .await?;
        }

        let stale_version = created.version.clone();
        let current = if matches!(failure, CleanupFailure::StaleVersion) {
            let mut changed_metadata = created.metadata.clone().expect("VPC metadata");
            changed_metadata.description = "changed concurrently".to_string();
            env.api
                .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
                    id: Some(vpc_id),
                    metadata: Some(changed_metadata),
                    ..Default::default()
                }))
                .await?
                .into_inner()
                .vpc
                .expect("updated VPC")
        } else {
            created.clone()
        };

        let request_version = match failure {
            CleanupFailure::MissingVersion => None,
            CleanupFailure::StaleVersion => Some(stale_version.clone()),
            CleanupFailure::MissingExpectedVni
            | CleanupFailure::InvalidExpectedVni(_)
            | CleanupFailure::UnexpectedInactiveVni
            | CleanupFailure::NoInactiveVni
            | CleanupFailure::ActiveAllocationMismatch
            | CleanupFailure::DuplicateInactiveVni => Some(current.version.clone()),
        };
        let pool_state_before = vpc_vni_pool_state(&env).await?;

        let expected_inactive_vni = match failure {
            CleanupFailure::MissingExpectedVni => None,
            CleanupFailure::InvalidExpectedVni(vni) => Some(vni),
            CleanupFailure::UnexpectedInactiveVni | CleanupFailure::NoInactiveVni => {
                Some(active_vni)
            }
            _ => inactive_vni,
        };

        let error = env
            .api
            .release_vpc_inactive_vni(tonic::Request::new(
                rpc::forge::VpcReleaseInactiveVniRequest {
                    id: Some(vpc_id),
                    if_version_match: request_version,
                    expected_inactive_vni,
                },
            ))
            .await
            .expect_err(scenario);
        let expected_code = match failure {
            CleanupFailure::MissingVersion
            | CleanupFailure::MissingExpectedVni
            | CleanupFailure::InvalidExpectedVni(_) => tonic::Code::InvalidArgument,
            CleanupFailure::StaleVersion
            | CleanupFailure::UnexpectedInactiveVni
            | CleanupFailure::NoInactiveVni
            | CleanupFailure::ActiveAllocationMismatch
            | CleanupFailure::DuplicateInactiveVni => tonic::Code::FailedPrecondition,
        };
        assert_eq!(error.code(), expected_code, "{scenario}: {error}");
        if matches!(failure, CleanupFailure::StaleVersion) {
            assert!(
                error.message().contains(&format!(
                    "did not have the expected version {stale_version}"
                )),
                "{scenario}: {error}"
            );
        }

        assert_eq!(find_test_vpc(&env, vpc_id).await?, current, "{scenario}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            pool_state_before,
            "{scenario}"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn vpc_deletion_requires_explicit_inactive_vni_release(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(pool).await;
    let (vpc_id, created) = create_fixture_vpc(
        &env,
        "delete after explicit cleanup".to_string(),
        None,
        None,
    )
    .await;
    let active_vni = i32::try_from(
        created
            .status
            .as_ref()
            .and_then(|status| status.vni)
            .expect("created VPC has an active VNI"),
    )?;
    let (peer_vpc_id, _) =
        create_fixture_vpc(&env, "preserved peer VPC".to_string(), None, None).await;
    let peering = env
        .api
        .create_vpc_peering(tonic::Request::new(rpc::forge::VpcPeeringCreationRequest {
            vpc_id: Some(vpc_id),
            peer_vpc_id: Some(peer_vpc_id),
            id: None,
        }))
        .await?
        .into_inner();

    let inactive_vni = allocate_external_vni(&env, vpc_id).await?;
    let sentinel_owner = VpcId::new();
    let sentinel_vni = allocate_external_vni(&env, sentinel_owner).await?;
    let pool_state_before = vpc_vni_pool_state(&env).await?;

    // There are no instances, but the retained allocation still requires
    // operator cleanup before deletion may remove the VPC or its peerings.
    let error = env
        .api
        .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
        .await
        .expect_err("deletion must not implicitly release a retained VNI");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(find_test_vpc(&env, vpc_id).await?, created);
    assert_eq!(vpc_vni_pool_state(&env).await?, pool_state_before);
    let peerings = env
        .api
        .find_vpc_peerings_by_ids(tonic::Request::new(rpc::forge::VpcPeeringsByIdsRequest {
            vpc_peering_ids: vec![peering.id.expect("created peering has an ID")],
        }))
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(peerings, vec![peering.clone()]);

    let released = env
        .api
        .release_vpc_inactive_vni(tonic::Request::new(
            rpc::forge::VpcReleaseInactiveVniRequest {
                id: Some(vpc_id),
                if_version_match: Some(created.version),
                expected_inactive_vni: Some(u32::try_from(inactive_vni)?),
            },
        ))
        .await?
        .into_inner();
    assert_eq!(released.released_inactive_vni, u32::try_from(inactive_vni)?);

    env.api
        .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
        .await?;
    assert!(
        env.api
            .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
            .await?
            .into_inner()
            .vpcs
            .is_empty()
    );
    assert!(
        env.api
            .find_vpc_peerings_by_ids(tonic::Request::new(rpc::forge::VpcPeeringsByIdsRequest {
                vpc_peering_ids: vec![peering.id.expect("created peering has an ID")],
            }))
            .await?
            .into_inner()
            .vpc_peerings
            .is_empty()
    );

    let internal_pool = env.common_pools.ethernet.pool_vpc_vni.name();
    let external_pool = env.common_pools.ethernet.pool_external_vpc_vni.name();
    for (pool_name, vni) in [(internal_pool, active_vni), (external_pool, inactive_vni)] {
        assert_eq!(
            resource_pool_entry_state(&env, pool_name, vni).await?,
            ResourcePoolEntryState::Free
        );
    }
    assert_eq!(
        resource_pool_entry_state(&env, external_pool, sentinel_vni).await?,
        ResourcePoolEntryState::Allocated {
            owner: sentinel_owner.to_string(),
            owner_type: OwnerType::Vpc.to_string(),
        }
    );

    Ok(())
}

#[crate::sqlx_test]
async fn vpc_deletion_rejects_inconsistent_owned_allocations(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    #[derive(Clone, Copy)]
    enum AllocationState {
        OnlyInactive,
        DuplicateActivePool,
    }

    let env = create_test_env(pool).await;
    // Create every VPC before releasing a lease that its live status still
    // references, so no later fixture can reallocate that VNI.
    let mut fixtures = Vec::new();
    for (scenario, state) in [
        ("only an inactive allocation", AllocationState::OnlyInactive),
        (
            "duplicate active-pool allocations",
            AllocationState::DuplicateActivePool,
        ),
    ] {
        let (vpc_id, created) = create_fixture_vpc(&env, scenario.to_string(), None, None).await;
        fixtures.push((scenario, state, vpc_id, created));
    }

    for (scenario, state, vpc_id, created) in fixtures {
        let mut txn = env.pool.begin().await?;
        let allocation_pool = match state {
            AllocationState::OnlyInactive => {
                let active_vni = created
                    .status
                    .as_ref()
                    .and_then(|status| status.vni)
                    .expect("created VPC has an active VNI");
                db::resource_pool::release(
                    &env.common_pools.ethernet.pool_vpc_vni,
                    &mut txn,
                    i32::try_from(active_vni)?,
                )
                .await?;
                &env.common_pools.ethernet.pool_external_vpc_vni
            }
            AllocationState::DuplicateActivePool => &env.common_pools.ethernet.pool_vpc_vni,
        };
        db::resource_pool::allocate(
            allocation_pool,
            &mut txn,
            OwnerType::Vpc,
            &vpc_id.to_string(),
            None,
        )
        .await?;
        txn.commit().await?;
        let pool_state_before = vpc_vni_pool_state(&env).await?;

        let error = env
            .api
            .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
            .await
            .expect_err(scenario);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert_eq!(find_test_vpc(&env, vpc_id).await?, created, "{scenario}");
        assert_eq!(
            vpc_vni_pool_state(&env).await?,
            pool_state_before,
            "{scenario}"
        );
    }

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
