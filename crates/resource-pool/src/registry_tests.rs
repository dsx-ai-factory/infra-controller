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

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::Ipv4Addr;

use db::resource_pool::binding::{self, BindingError};
use db::resource_pool::populate;
use model::resource_pool::binding::{
    INTEGRATED_AUTHORITY_ID, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain,
};
use model::resource_pool::common::ib_pkey_pool_name;
use model::resource_pool::{
    OwnerType, Range, ResourcePool, ResourcePoolDef, ResourcePoolType, ValueType,
};

use crate::{
    PoolDefinitionSource, ResourcePoolBackend, ResourcePoolRegistry, ResourcePoolRegistryError,
};

fn integrated_selection(
    name: &str,
    value_domain: ResourcePoolValueDomain,
) -> ResourcePoolSelection {
    ResourcePoolSelection {
        pool_name: name.to_string(),
        value_domain,
        backend_name: None,
        backend_kind: ResourcePoolBackendKind::Integrated,
        authority_id: INTEGRATED_AUTHORITY_ID.to_string(),
        remote_pool: name.to_string(),
    }
}

#[carbide_macros::sqlx_test]
async fn runtime_lookup_borrows_resolved_backend_without_database_access(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let selection = integrated_selection("resolved", ResourcePoolValueDomain::Integer);
    let configured = HashMap::from([(selection.pool_name.clone(), selection)]);
    let mut txn = database.begin().await?;
    let registry = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await?;
    txn.commit().await?;

    // Any SQL lookup would fail in this aborted transaction.
    let mut txn = database.begin().await?;
    sqlx::query("SELECT 1 / 0")
        .execute(&mut *txn)
        .await
        .expect_err("division by zero must abort the transaction");
    let Some(Cow::Borrowed(backend)) = registry.get_or_load_backend(&mut txn, "resolved").await?
    else {
        panic!("a resolved backend must be borrowed without querying the database");
    };
    assert!(std::ptr::eq(
        backend,
        &registry.get("resolved").expect("resolved pool").backend
    ));
    txn.rollback().await?;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn runtime_lookup_uses_the_callers_transaction_without_caching_results(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ResourcePoolRegistry::default();
    let selection = integrated_selection("late-pool", ResourcePoolValueDomain::Integer);
    let mut txn = database.begin().await?;
    assert!(
        registry
            .get_or_load_backend(&mut txn, &selection.pool_name)
            .await?
            .is_none()
    );
    assert!(binding::all(&mut *txn).await?.is_empty());

    binding::bind_or_verify(&mut txn, &selection).await?;
    assert!(matches!(
        registry
            .get_or_load_backend(&mut txn, &selection.pool_name)
            .await?,
        Some(Cow::Owned(ResourcePoolBackend::Integrated(_)))
    ));
    assert!(registry.get(&selection.pool_name).is_none());
    txn.rollback().await?;

    // A backend loaded from a rolled-back binding must not survive in memory.
    let mut txn = database.begin().await?;
    assert!(
        registry
            .get_or_load_backend(&mut txn, &selection.pool_name)
            .await?
            .is_none()
    );
    txn.commit().await?;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn runtime_lookup_uses_unbound_sql_values_without_saving_or_caching_a_binding(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ResourcePoolRegistry::default();
    let pool = ResourcePool::new("late-unbound".to_string(), ValueType::Integer);
    let mut txn = database.begin().await?;

    // A replica that predates pool bindings can grow and allocate from this pool
    // after the new replica has built its registry. Allocated values count too:
    // their owners still need to release them through Integrated.
    populate(&pool, &mut txn, vec![42], true).await?;
    db::resource_pool::allocate_exact(&pool, &mut txn, OwnerType::Vpc, "vpc-1", 42).await?;
    assert!(matches!(
        registry.get_or_load_backend(&mut txn, pool.name()).await?,
        Some(Cow::Owned(ResourcePoolBackend::Integrated(_)))
    ));
    assert!(binding::all(&mut *txn).await?.is_empty());
    assert!(registry.get(pool.name()).is_none());
    txn.rollback().await?;

    // The rolled-back values must not leave an Integrated backend in memory.
    let mut txn = database.begin().await?;
    assert!(
        registry
            .get_or_load_backend(&mut txn, pool.name())
            .await?
            .is_none()
    );
    assert!(binding::all(&mut *txn).await?.is_empty());
    txn.commit().await?;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn runtime_lookup_rejects_saved_authority_before_remote_alias(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ResourcePoolRegistry::default();
    let remote = ResourcePoolSelection {
        backend_kind: ResourcePoolBackendKind::Grpc,
        authority_id: "provider-a".to_string(),
        remote_pool: "provider-pool".to_string(),
        ..integrated_selection("remote", ResourcePoolValueDomain::Integer)
    };
    let mut txn = database.begin().await?;
    let original = binding::bind_or_verify(&mut txn, &remote).await?;
    let local_pool = ResourcePool::new(remote.pool_name.clone(), ValueType::Integer);
    populate(&local_pool, &mut txn, vec![42], true).await?;
    txn.commit().await?;

    let mut txn = database.begin().await?;
    let Err(error) = registry
        .get_or_load_backend(&mut txn, &remote.pool_name)
        .await
    else {
        panic!("a saved unsupported authority must be rejected");
    };
    assert!(matches!(
        error,
        ResourcePoolRegistryError::UnsupportedAuthority { name } if name == remote.pool_name
    ));
    txn.commit().await?;
    assert_eq!(binding::all(&database).await?, vec![original]);
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn bootstrap_and_restart_retain_legacy_and_unpopulated_pool_bindings(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = ResourcePool::new("legacy-vni".to_string(), ValueType::Integer);
    let pkey = ResourcePoolSelection {
        backend_name: Some("database".to_string()),
        ..integrated_selection(
            &ib_pkey_pool_name("default"),
            ResourcePoolValueDomain::Integer,
        )
    };
    let configured = HashMap::from([(pkey.pool_name.clone(), pkey.clone())]);
    let mut txn = database.begin().await?;
    populate(&legacy, &mut txn, vec![42], true).await?;
    let registry = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await?;
    txn.commit().await?;

    for expected in [
        integrated_selection(legacy.name(), ResourcePoolValueDomain::Integer),
        pkey,
    ] {
        let resolved = registry
            .get(&expected.pool_name)
            .expect("configured and legacy pools must both resolve");
        assert!(resolved.binding.matches(&expected));
        assert_eq!(resolved.binding.backend_name, expected.backend_name);
        assert!(matches!(
            &resolved.backend,
            ResourcePoolBackend::Integrated(_)
        ));
    }
    let original_bindings = binding::all(&database).await?;
    assert_eq!(original_bindings.len(), 2);
    drop(registry);

    let mut txn = database.begin().await?;
    let restarted = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &HashMap::new(),
    )
    .await?;
    txn.commit().await?;
    for original in &original_bindings {
        let resolved = restarted
            .get(&original.pool_name)
            .expect("removing declarations must retain the binding");
        assert_eq!(&resolved.binding, original);
        assert!(matches!(
            &resolved.backend,
            ResourcePoolBackend::Integrated(_)
        ));
    }
    assert_eq!(binding::all(&database).await?, original_bindings);
    let inventory: Vec<(String, String, ValueType)> =
        sqlx::query_as("SELECT name, value, value_type FROM resource_pool ORDER BY name, value")
            .fetch_all(&database)
            .await?;
    assert_eq!(
        inventory,
        vec![(
            legacy.name().to_string(),
            "42".to_string(),
            ValueType::Integer
        )]
    );
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn listen_only_adopts_legacy_pools_and_defers_new_bindings(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = ResourcePool::new("legacy".to_string(), ValueType::Integer);
    let snapshot = ResourcePoolDef {
        ranges: vec![],
        prefix: Some("2001:db8::/64".to_string()),
        pool_type: ResourcePoolType::Ipv6,
        delegate_prefix_len: None,
    };
    let retained = integrated_selection("retained", ResourcePoolValueDomain::Integer);
    let configured_only = integrated_selection("new-pool", ResourcePoolValueDomain::Integer);
    let configured = HashMap::from([(configured_only.pool_name.clone(), configured_only)]);
    let mut txn = database.begin().await?;
    populate(&legacy, &mut txn, vec![42], true).await?;
    db::resource_pool::insert_pool_def(&mut txn, "snapshot", &snapshot).await?;
    let original = binding::bind_or_verify(&mut txn, &retained).await?;
    txn.commit().await?;

    let mut txn = database.begin().await?;
    let listening =
        ResourcePoolRegistry::resolve(PoolDefinitionSource::ListenOnly, &mut txn, &configured)
            .await?;
    txn.commit().await?;

    let persisted_bindings = binding::all(&database).await?;
    for expected in [
        integrated_selection(legacy.name(), ResourcePoolValueDomain::Integer),
        integrated_selection("snapshot", ResourcePoolValueDomain::IpAddress),
    ] {
        let persisted = persisted_bindings
            .iter()
            .find(|binding| binding.pool_name == expected.pool_name)
            .expect("listen-only startup must adopt existing inventory and snapshots");
        assert!(persisted.matches(&expected));
        assert_eq!(
            &listening
                .get(&expected.pool_name)
                .expect("adopted pool must be available in the registry")
                .binding,
            persisted
        );
    }
    assert_eq!(
        listening
            .get(&original.pool_name)
            .expect("retained binding must be available without inventory")
            .binding,
        original
    );
    assert_eq!(persisted_bindings.len(), 3);
    assert!(listening.get("new-pool").is_none());
    assert!(
        persisted_bindings
            .iter()
            .all(|binding| binding.pool_name != "new-pool")
    );
    drop(listening);

    // An ignored declaration must not fix the domain before a process with
    // authoritative seed configuration creates this pool's first binding.
    let authoritative = integrated_selection("new-pool", ResourcePoolValueDomain::IpAddress);
    let configured = HashMap::from([(authoritative.pool_name.clone(), authoritative.clone())]);
    let mut txn = database.begin().await?;
    let seeded = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await?;
    txn.commit().await?;

    let first = &seeded
        .get(&authoritative.pool_name)
        .expect("authoritative configuration must establish the first binding")
        .binding;
    assert!(first.matches(&authoritative));
    assert_eq!(
        binding::all(&database)
            .await?
            .into_iter()
            .find(|binding| binding.pool_name == authoritative.pool_name),
        Some(first.clone())
    );
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn seed_domain_drift_preserves_inventory_and_binding_but_growth_requires_matching_domain(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = ResourcePool::new("drifted".to_string(), ValueType::Integer);
    let definition = ResourcePoolDef {
        ranges: vec![Range {
            start: "42".to_string(),
            end: "43".to_string(),
            auto_assign: true,
        }],
        prefix: None,
        pool_type: ResourcePoolType::Integer,
        delegate_prefix_len: None,
    };
    let mut txn = database.begin().await?;
    populate(&legacy, &mut txn, vec![42], true).await?;
    db::resource_pool::insert_pool_def(&mut txn, legacy.name(), &definition).await?;
    txn.commit().await?;

    let inventory_and_snapshot = "SELECT
        (SELECT to_jsonb(resource_pool)::text FROM resource_pool),
        (SELECT to_jsonb(resource_pool_def)::text FROM resource_pool_def)";
    let original_rows: (String, String) = sqlx::query_as(inventory_and_snapshot)
        .fetch_one(&database)
        .await?;
    let configured = HashMap::from([(
        legacy.name().to_string(),
        integrated_selection(legacy.name(), ResourcePoolValueDomain::IpAddress),
    )]);
    let mut txn = database.begin().await?;
    let first = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await?;
    txn.commit().await?;
    let original_binding = first
        .get(legacy.name())
        .expect("legacy pool binding")
        .binding
        .clone();
    assert_eq!(
        original_binding.value_domain,
        ResourcePoolValueDomain::Integer
    );
    drop(first);

    let mut txn = database.begin().await?;
    let restarted = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(
        restarted
            .get(legacy.name())
            .expect("retained pool binding")
            .binding,
        original_binding
    );
    let retained_rows: (String, String) = sqlx::query_as(inventory_and_snapshot)
        .fetch_one(&database)
        .await?;
    assert_eq!(retained_rows, original_rows);

    let mut growth = configured;
    let new = integrated_selection("a-new", ResourcePoolValueDomain::Integer);
    growth.insert(new.pool_name.clone(), new.clone());
    let mut txn = database.begin().await?;
    let Err(error) =
        ResourcePoolRegistry::resolve(PoolDefinitionSource::GrowthRequest, &mut txn, &growth).await
    else {
        panic!("explicit growth must reject the seed's mismatched domain");
    };
    assert!(matches!(
        error,
        ResourcePoolRegistryError::ValueDomainConflict {
            name,
            stored: ResourcePoolValueDomain::Integer,
            configured: ResourcePoolValueDomain::IpAddress,
        } if name == legacy.name()
    ));
    // Commit the error path so rollback cannot hide a prospective binding write.
    txn.commit().await?;
    assert_eq!(
        binding::all(&database).await?,
        vec![original_binding.clone()]
    );
    let retained_rows: (String, String) = sqlx::query_as(inventory_and_snapshot)
        .fetch_one(&database)
        .await?;
    assert_eq!(retained_rows, original_rows);

    growth
        .get_mut(legacy.name())
        .expect("growth includes the existing pool")
        .value_domain = ResourcePoolValueDomain::Integer;
    let mut txn = database.begin().await?;
    let grown =
        ResourcePoolRegistry::resolve(PoolDefinitionSource::GrowthRequest, &mut txn, &growth)
            .await?;
    txn.commit().await?;
    assert_eq!(
        grown
            .get(legacy.name())
            .expect("matching growth must retain the existing binding")
            .binding,
        original_binding
    );
    let new_binding = &grown
        .get(&new.pool_name)
        .expect("matching growth must bind the prospective pool")
        .binding;
    assert!(new_binding.matches(&new));
    assert_eq!(
        binding::all(&database).await?,
        vec![new_binding.clone(), original_binding]
    );
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn invalid_existing_selection_prevents_every_prospective_binding_write(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing = integrated_selection("z-existing", ResourcePoolValueDomain::Integer);
    let mut txn = database.begin().await?;
    let original = binding::bind_or_verify(&mut txn, &existing).await?;
    txn.commit().await?;

    // The new name sorts first, so validating during insertion would leave it behind.
    let new = integrated_selection("a-new", ResourcePoolValueDomain::Integer);
    let invalid = ResourcePoolSelection {
        value_domain: ResourcePoolValueDomain::IpAddress,
        ..existing
    };
    let configured = HashMap::from([
        (new.pool_name.clone(), new),
        (invalid.pool_name.clone(), invalid),
    ]);
    let mut txn = database.begin().await?;
    let Err(error) = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await
    else {
        panic!("changing the stored pool domain must fail");
    };
    assert!(matches!(
        error,
        ResourcePoolRegistryError::Binding(BindingError::Conflict { name, .. })
            if name == original.pool_name
    ));

    // Commit the failed call's transaction so rollback cannot hide partial writes.
    txn.commit().await?;
    assert_eq!(binding::all(&database).await?, vec![original]);
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn grpc_authorities_fail_before_binding_writes_and_legacy_inventory_classification(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let remote = ResourcePoolSelection {
        backend_kind: ResourcePoolBackendKind::Grpc,
        authority_id: "provider-a".to_string(),
        ..integrated_selection("remote", ResourcePoolValueDomain::Integer)
    };
    let new = integrated_selection("a-new", ResourcePoolValueDomain::Integer);
    let configured = HashMap::from([
        (new.pool_name.clone(), new.clone()),
        (remote.pool_name.clone(), remote.clone()),
    ]);
    let mut txn = database.begin().await?;
    let Err(error) = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await
    else {
        panic!("a configured remote authority must fail before binding any pool");
    };
    assert!(matches!(
        error,
        ResourcePoolRegistryError::UnsupportedAuthority { name } if name == remote.pool_name
    ));
    // Commit the error path so rollback cannot hide a prospective binding write.
    txn.commit().await?;
    assert!(binding::all(&database).await?.is_empty());

    let mut txn = database.begin().await?;
    let original = binding::bind_or_verify(&mut txn, &remote).await?;

    // An unrelated legacy pool has mixed row types. The retained remote binding
    // must still report the unsupported authority before examining those rows.
    let legacy_integer = ResourcePool::new("legacy".to_string(), ValueType::Integer);
    let legacy_ipv4 = ResourcePool::new("legacy".to_string(), ValueType::Ipv4);
    populate(&legacy_integer, &mut txn, vec![7], true).await?;
    populate(
        &legacy_ipv4,
        &mut txn,
        vec![Ipv4Addr::new(192, 0, 2, 1)],
        true,
    )
    .await?;
    txn.commit().await?;

    let remote_inventory: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM resource_pool WHERE name = $1")
            .bind(&remote.pool_name)
            .fetch_one(&database)
            .await?;
    assert_eq!(remote_inventory, 0);
    let configured = HashMap::from([(new.pool_name.clone(), new)]);
    let mut txn = database.begin().await?;
    let Err(error) = ResourcePoolRegistry::resolve(
        PoolDefinitionSource::SeedConfiguration,
        &mut txn,
        &configured,
    )
    .await
    else {
        panic!("a retained remote authority must fail without local inventory");
    };
    assert!(matches!(
        error,
        ResourcePoolRegistryError::UnsupportedAuthority { name } if name == remote.pool_name
    ));
    txn.commit().await?;
    assert_eq!(binding::all(&database).await?, vec![original]);
    Ok(())
}
