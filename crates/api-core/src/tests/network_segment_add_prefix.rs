/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::HashMap;

use carbide_uuid::network::NetworkSegmentId;
use model::allocation_type::AllocationType;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{AllocationStrategy, NetworkDefinition, NetworkDefinitionSegmentType};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides,
};
use crate::tests::machine_dhcp::{
    RPC_MESSAGE_KIND_V6_INFO_REQUEST, RPC_MESSAGE_KIND_V6_SOLICIT, dhcpv6_discovery,
    enable_slaac_eui64_inference, expected_slaac_address,
};

/// Builds an Underlay definition for startup fixtures, so callers can compare
/// its immutable seed with the authoritative prefix rows after mutation.
fn underlay_seed(prefix: &str, gateway: Option<&str>) -> NetworkDefinition {
    NetworkDefinition {
        segment_type: NetworkDefinitionSegmentType::Underlay,
        prefix: prefix.parse().expect("fixture CIDR"),
        prefix_v6: None,
        gateway: gateway.map(|address| address.parse().expect("fixture gateway")),
        dhcpv6_link_address: None,
        mtu: 1500,
        reserve_first: 5,
        allocation_strategy: Default::default(),
        infer_slaac_eui64_addresses: false,
        vpc_name: None,
    }
}

/// Reloads through the public reader to compare persisted config and lifecycle,
/// rather than trusting a mutation's response or a stale model.
async fn read_segment(api: &crate::api::Api, id: NetworkSegmentId) -> forge::NetworkSegment {
    api.find_network_segments_by_ids(Request::new(forge::NetworkSegmentsByIdsRequest {
        network_segments_ids: vec![id],
        ..Default::default()
    }))
    .await
    .expect("read segment")
    .into_inner()
    .network_segments
    .pop()
    .expect("segment exists")
}

/// Reads the config version from its public lifecycle location. The separate
/// controller-state version must not advance for a prefix-only mutation.
fn config_version(segment: &forge::NetworkSegment) -> config_version::ConfigVersion {
    segment
        .status
        .as_ref()
        .expect("status")
        .lifecycle
        .as_ref()
        .expect("lifecycle")
        .version
        .parse()
        .expect("valid stored config version")
}

/// Adding IPv6 to an occupied BMC underlay must leave its existing machine,
/// interface and IPv4 lease intact, then allow inference and DHCPv6 on that
/// same interface using the newly stored prefix.
#[crate::sqlx_test]
async fn added_ipv6_prefix_serves_existing_bmc(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = create_test_env(pool.clone()).await;
    let host = create_managed_host(&env).await;
    let mut txn = pool.begin().await?;
    let bmc = db::machine_interface::find_all(txn.as_mut())
        .await?
        .into_iter()
        .find(|interface| {
            interface.machine_id == Some(host.host().id.into())
                && interface.interface_type == model::machine_interface::InterfaceType::Bmc
        })
        .expect("linked host BMC");
    let before_addresses =
        db::machine_interface_address::find_for_interface(&mut txn, bmc.id).await?;
    assert!(!before_addresses.is_empty(), "occupied BMC fixture");
    assert!(
        before_addresses
            .iter()
            .all(|address| address.address.is_ipv4())
    );
    txn.commit().await?;
    // Enable the existing policy before append; the new prefix must immediately
    // participate in inference without rewriting the segment or its policy.
    enable_slaac_eui64_inference(&pool, bmc.segment_id)
        .await
        .expect("enable SLAAC inference");
    let before = read_segment(&env.api, bmc.segment_id).await;
    env.api
        .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
            network_segment_id: Some(bmc.segment_id),
            prefix: "2001:db8:53::/64".to_string(),
            dhcpv6_link_address: Some("2001:db8:ffff::53".to_string()),
            reserve_first: 5,
            ..Default::default()
        }))
        .await?;
    let stored = read_segment(&env.api, bmc.segment_id).await;
    assert!(
        stored
            .config
            .as_ref()
            .expect("config")
            .prefixes
            .contains(&before.config.as_ref().expect("config").prefixes[0],)
    );
    env.api
        .discover_dhcp(dhcpv6_discovery(
            bmc.mac_address,
            "2001:db8:ffff::53",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?;
    let mut txn = pool.begin().await?;
    let inferred = db::machine_interface_address::find_for_interface(&mut txn, bmc.id)
        .await?
        .into_iter()
        .find(|address| address.address.is_ipv6())
        .expect("IPv6 address inferred from the appended prefix");
    assert_eq!(inferred.allocation_type, AllocationType::Slaac);
    assert_eq!(
        inferred.address,
        expected_slaac_address("2001:db8:53::".parse()?, bmc.mac_address)
    );
    txn.commit().await?;
    // The new prefix must be usable by the real family-specific DHCP path,
    // including its out-of-prefix relay selector.
    let lease = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            bmc.mac_address,
            "2001:db8:ffff::53",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(lease.machine_interface_id, Some(bmc.id));
    assert_eq!(lease.segment_id, Some(bmc.segment_id));
    assert_eq!(lease.prefix, "2001:db8:53::/64");
    let mut txn = pool.begin().await?;
    let after = db::machine_interface::find_one(txn.as_mut(), bmc.id).await?;
    assert_eq!(after.machine_id, bmc.machine_id);
    assert_eq!(after.attached_dpu_machine_id, bmc.attached_dpu_machine_id);
    assert_eq!(after.segment_id, bmc.segment_id);
    assert_eq!(after.primary_interface, bmc.primary_interface);
    assert_eq!(after.interface_type, bmc.interface_type);
    let addresses = db::machine_interface_address::find_for_interface(&mut txn, bmc.id).await?;
    assert_eq!(addresses.len(), before_addresses.len() + 1);
    for old in before_addresses {
        assert!(
            addresses
                .iter()
                .any(|address| address.address == old.address
                    && address.allocation_type == old.allocation_type)
        );
    }
    assert!(
        addresses
            .iter()
            .any(|address| address.address.to_string() == lease.address)
    );
    txn.commit().await?;
    Ok(())
}

/// Both addition directions preserve identities, lifecycle and seed data. A
/// restart must read the added prefix even when TOML retains the original seed.
#[crate::sqlx_test]
async fn added_prefixes_survive_retry_and_startup(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::no_network_segments()).await;
    for (name, seed, added, gateway, relay) in [
        // IPv6 relay metadata must be returned from storage, not dropped by conversion.
        (
            "add-v6",
            NetworkDefinition {
                // Appending a family must not turn reserved-only DHCP into dynamic allocation.
                allocation_strategy: AllocationStrategy::Reserved,
                ..underlay_seed("192.0.44.0/24", Some("192.0.44.1"))
            },
            "2001:db8:44::/64",
            None,
            Some("2001:db8:ffff::44"),
        ),
        // The reverse direction must not depend on the old prefix having a gateway.
        (
            "add-v4",
            NetworkDefinition {
                // An IPv4 addition must preserve the IPv6 seed's relay selector.
                dhcpv6_link_address: Some("2001:db8:ffff::45".parse()?),
                // Enabled inference must survive both append and seed reconciliation.
                infer_slaac_eui64_addresses: true,
                ..underlay_seed("2001:db8:45::/64", None)
            },
            "192.0.45.0/24",
            Some("192.0.45.1"),
            None,
        ),
    ] {
        let seeds = HashMap::from([(name.to_string(), seed.clone())]);
        crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
        let mut txn = pool.begin().await?;
        let stored = db::network_segment::find_by_name(&mut txn, name).await?;
        txn.commit().await?;
        let before = read_segment(&env.api, stored.id).await;
        let request = forge::AddNetworkSegmentPrefixRequest {
            network_segment_id: Some(stored.id),
            prefix: added.to_string(),
            gateway: gateway.map(str::to_string),
            reserve_first: 7,
            dhcpv6_link_address: relay.map(str::to_string),
        };
        let applied = env
            .api
            .add_network_segment_prefix(Request::new(request.clone()))
            .await?
            .into_inner();
        assert_eq!(
            config_version(&applied).version_nr(),
            stored.version.increment().version_nr()
        );
        assert_eq!(applied.id, before.id);
        let mut expected_status = before.status.clone();
        expected_status
            .as_mut()
            .expect("status")
            .lifecycle
            .as_mut()
            .expect("lifecycle")
            .version = config_version(&applied).version_string();
        assert_eq!(applied.status, expected_status);
        assert_eq!(applied.metadata, before.metadata);
        let old_prefix = &before.config.as_ref().expect("config").prefixes[0];
        let config = applied.config.as_ref().expect("config");
        assert_eq!(config.prefixes.len(), 2);
        assert!(config.prefixes.contains(old_prefix));
        let new_prefix = config
            .prefixes
            .iter()
            .find(|prefix| prefix.prefix == added)
            .expect("added prefix");
        assert!(new_prefix.id.is_some());
        assert_ne!(new_prefix.id, old_prefix.id);
        assert_eq!(new_prefix.gateway.as_deref(), gateway);
        assert_eq!(new_prefix.dhcpv6_link_address.as_deref(), relay);
        assert_eq!(new_prefix.reserve_first, 7);
        assert_eq!(
            config.infer_slaac_eui64_addresses,
            seed.infer_slaac_eui64_addresses
        );
        // Allocation strategy is stored policy, not part of the public config.
        let mut txn = pool.begin().await?;
        let persisted = db::network_segment::find_by_name(&mut txn, name).await?;
        assert_eq!(
            persisted.config.allocation_strategy,
            stored.config.allocation_strategy
        );
        txn.commit().await?;

        // A repeated successful request is read-only, including timestamps/version.
        let retry = env
            .api
            .add_network_segment_prefix(Request::new(request.clone()))
            .await?
            .into_inner();
        assert_eq!(retry, applied);
        assert_eq!(read_segment(&env.api, stored.id).await, applied);

        // CIDR/reservation equality cannot hide a changed gateway or relay selector.
        let mut conflicting = request;
        if gateway.is_some() {
            conflicting.gateway = Some("192.0.45.2".to_string());
        } else {
            conflicting.dhcpv6_link_address = Some("2001:db8:ffff::144".to_string());
        }
        let error = env
            .api
            .add_network_segment_prefix(Request::new(conflicting))
            .await
            .expect_err("same CIDR with changed gateway or relay");
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert!(error.message().contains("already has"), "{error}");
        assert_eq!(read_segment(&env.api, stored.id).await, applied);

        // Startup must not overwrite live prefixes with the single-family seed.
        crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
        assert_eq!(read_segment(&env.api, stored.id).await, applied);
        let mut txn = pool.begin().await?;
        assert_eq!(
            db::network_segment::stored_def(txn.as_mut(), name).await?,
            Some(seed)
        );
        let persisted = db::network_segment::find_by_name(&mut txn, name).await?;
        assert_eq!(
            persisted.config.allocation_strategy,
            stored.config.allocation_strategy
        );
        assert_eq!(
            persisted.config.infer_slaac_eui64_addresses,
            stored.config.infer_slaac_eui64_addresses
        );
        assert_eq!(
            persisted.status.controller_state.version,
            stored.status.controller_state.version
        );
        let zone = db::dns::cidr_to_reverse_zone(added.parse()?).expect("aligned prefix");
        assert_eq!(
            db::dns::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone)
                .await?
                .len(),
            1
        );
        txn.commit().await?;
    }
    Ok(())
}

/// Invalid targets and conflicting requests must fail without modifying even
/// the version. Cases distinguish eligibility, family and global overlap rules.
#[crate::sqlx_test]
async fn add_prefix_rejections_preserve_segment(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::no_network_segments().with_fnn_config(None),
    )
    .await;
    let tenant = "append-overlap";
    crate::tests::common::api_fixtures::tenant::create_fixture_tenant(&env, tenant).await?;
    env.api
        .create_vpc(
            forge::VpcCreationRequest::builder(tenant.to_string())
                .metadata(forge::Metadata {
                    name: "attached-target".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?;
    let mut occupied = underlay_seed("2001:db8:47::/64", None);
    // Relay uniqueness is independent of prefix-range overlap.
    occupied.dhcpv6_link_address = Some("2001:db8:ffff::47".parse()?);
    let seeds = HashMap::from([
        (
            "target".to_string(),
            underlay_seed("192.0.46.0/24", Some("192.0.46.1")),
        ),
        ("occupied".to_string(), occupied),
    ]);
    crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
    // Scoped VPC parents require IPv4 and exact SitePrefix lineage. Reuse the
    // ready tenant-parent fixture rather than inventing an impossible IPv6 row.
    let scoped_parent = crate::tests::vpc_prefix::seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "192.168.247.0/24",
        model::site_prefix::SitePrefixLifecycleState::Ready,
    )
    .await;
    let mut txn = pool.begin().await?;
    let target = db::network_segment::find_by_name(&mut txn, "target").await?;
    let ipv6_target = db::network_segment::find_by_name(&mut txn, "occupied").await?;
    let vpc = db::vpc::find_by_name(txn.as_mut(), "attached-target")
        .await?
        .pop()
        .expect("VPC fixture");
    // A stored VPC-scoped parent is separate from NetworkPrefix constraints;
    // this fixture tests the append probe, not VPC admission/allocation policy.
    db::vpc_prefix::persist(
        model::vpc_prefix::NewVpcPrefix {
            id: carbide_uuid::vpc::VpcPrefixId::new(),
            site_prefix_id: Some(scoped_parent),
            vpc_id: vpc.id,
            overlap_vpc_id: Some(vpc.id),
            config: model::vpc_prefix::VpcPrefixConfig {
                prefix: "192.168.247.0/24".parse()?,
            },
            metadata: model::metadata::Metadata {
                name: "scoped-overlap".to_string(),
                ..Default::default()
            },
        },
        vpc.version,
        &mut txn,
    )
    .await?;
    // An unattached underlay may still overlap a global VPC parent. Keep this
    // independent of the scoped parent so a blanket VPC-overlap ban would fail.
    // Persist advances the VPC's timestamped version; reread its actual token.
    let vpc = db::vpc::find_by_name(txn.as_mut(), "attached-target")
        .await?
        .pop()
        .expect("updated VPC fixture");
    db::vpc_prefix::persist(
        model::vpc_prefix::NewVpcPrefix {
            id: carbide_uuid::vpc::VpcPrefixId::new(),
            site_prefix_id: None,
            vpc_id: vpc.id,
            overlap_vpc_id: None,
            config: model::vpc_prefix::VpcPrefixConfig {
                prefix: "192.168.248.0/24".parse()?,
            },
            metadata: model::metadata::Metadata {
                name: "global-overlap".to_string(),
                ..Default::default()
            },
        },
        vpc.version,
        &mut txn,
    )
    .await?;
    txn.commit().await?;
    // Only the add-IPv4 direction can encounter a VPC-scoped parent. The
    // NetworkPrefix-table constraints cannot replace this cross-table probe.
    let before = read_segment(&env.api, ipv6_target.id).await;
    for (prefix, gateway, diagnostic) in [
        // Strict PostgreSQL gateway containment rejects an equal /32 network.
        ("192.0.60.7/32", "192.0.60.7", "shorter than /32"),
        // A scoped parent must reject even without a child NetworkPrefix row.
        ("192.168.247.0/24", "192.168.247.1", "overlap"),
    ] {
        let error = env
            .api
            .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
                network_segment_id: Some(ipv6_target.id),
                prefix: prefix.to_string(),
                gateway: Some(gateway.to_string()),
                ..Default::default()
            }))
            .await
            .expect_err(diagnostic);
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(error.message().contains(diagnostic), "{error}");
        assert_eq!(read_segment(&env.api, ipv6_target.id).await, before);
    }
    // The permitted global-parent overlap must save exactly one new prefix/version.
    let applied = env
        .api
        .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
            network_segment_id: Some(ipv6_target.id),
            prefix: "192.168.248.0/24".to_string(),
            gateway: Some("192.168.248.1".to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner();
    assert_eq!(applied.id, before.id);
    assert_eq!(
        config_version(&applied).version_nr(),
        config_version(&before).increment().version_nr()
    );
    let config = applied.config.as_ref().expect("config");
    assert_eq!(config.prefixes.len(), 2);
    assert!(
        config
            .prefixes
            .contains(&before.config.as_ref().expect("config").prefixes[0])
    );
    assert!(config.prefixes.iter().any(|prefix| {
        prefix.prefix == "192.168.248.0/24"
            && prefix.gateway.as_deref() == Some("192.168.248.1")
            && prefix.id.is_some()
    }));
    assert_eq!(read_segment(&env.api, ipv6_target.id).await, applied);
    for (intent, mutation, prefix, gateway, relay, code, diagnostic) in [
        // Same-family CIDR changes are not an additive operation.
        (
            "same family",
            None,
            "192.0.48.0/24",
            Some("192.0.48.1"),
            None,
            Code::FailedPrecondition,
            "already has",
        ),
        // Matching the CIDR alone is insufficient: the seed reserves five addresses.
        (
            "same CIDR with changed reservation",
            None,
            "192.0.46.0/24",
            Some("192.0.46.1"),
            None,
            Code::FailedPrecondition,
            "already has",
        ),
        // Another segment's range is unavailable even though the target lacks IPv6.
        (
            "overlap",
            None,
            "2001:db8:47::/80",
            None,
            None,
            Code::InvalidArgument,
            "overlaps",
        ),
        // Distinct ranges cannot share an authoritative relay link-address.
        (
            "relay conflict",
            None,
            "2001:db8:46::/64",
            None,
            Some("2001:db8:ffff::47"),
            Code::InvalidArgument,
            "link-address conflicts",
        ),
        // Host/admin/tenant projection is deliberately outside this mutation.
        (
            "wrong type",
            Some("UPDATE network_segments SET network_segment_type='admin' WHERE name='target'"),
            "2001:db8:46::/64",
            None,
            None,
            Code::FailedPrecondition,
            "underlay",
        ),
        // Even an Underlay attached to a VPC is outside the infrastructure mutation.
        (
            "VPC attached",
            Some(
                "UPDATE network_segments SET vpc_id=(SELECT id FROM vpcs WHERE name='attached-target') WHERE name='target'",
            ),
            "2001:db8:46::/64",
            None,
            None,
            Code::FailedPrecondition,
            "unattached",
        ),
        // Lifecycle updates cannot be bypassed by an operator append.
        (
            "not ready",
            Some(
                "UPDATE network_segments SET controller_state='{\"state\":\"provisioning\"}' WHERE name='target'",
            ),
            "2001:db8:46::/64",
            None,
            None,
            Code::FailedPrecondition,
            "ready",
        ),
        // Deletion is distinguished from an unknown identifier and cannot revive a segment.
        (
            "deleted",
            Some("UPDATE network_segments SET deleted=NOW() WHERE name='target'"),
            "2001:db8:46::/64",
            None,
            None,
            Code::FailedPrecondition,
            "live",
        ),
        // Prefixless segments cannot be created through append. Keep this last:
        // unlike the field mutations above, deleting its prefix is not reset.
        (
            "no existing family",
            Some(
                "DELETE FROM network_prefixes WHERE segment_id=(SELECT id FROM network_segments WHERE name='target')",
            ),
            "2001:db8:46::/64",
            None,
            None,
            Code::FailedPrecondition,
            "exactly one",
        ),
    ] {
        // Restore the target fields so only this row's mutation affects eligibility.
        sqlx::query("UPDATE network_segments SET network_segment_type='underlay', controller_state='{\"state\":\"ready\"}', deleted=NULL, vpc_id=NULL WHERE id=$1")
            .bind(target.id).execute(&pool).await?;
        if let Some(mutation) = mutation {
            sqlx::query(mutation).execute(&pool).await?;
        }
        let before = read_segment(&env.api, target.id).await;
        let error = env
            .api
            .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
                network_segment_id: Some(target.id),
                prefix: prefix.to_string(),
                gateway: gateway.map(str::to_string),
                dhcpv6_link_address: relay.map(str::to_string),
                ..Default::default()
            }))
            .await
            .expect_err(intent);
        assert_eq!(error.code(), code, "{intent}: {error}");
        assert!(error.message().contains(diagnostic), "{intent}: {error}");
        assert_eq!(read_segment(&env.api, target.id).await, before, "{intent}");
    }
    let error = env
        .api
        .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
            network_segment_id: Some(uuid::Uuid::new_v4().into()),
            prefix: "2001:db8:46::/64".to_string(),
            ..Default::default()
        }))
        .await
        .expect_err("unknown segment");
    assert_eq!(error.code(), Code::NotFound);
    let error = env
        .api
        .add_network_segment_prefix(Request::new(forge::AddNetworkSegmentPrefixRequest {
            prefix: "2001:db8:46::/64".to_string(),
            ..Default::default()
        }))
        .await
        .expect_err("missing target ID");
    assert_eq!(error.code(), Code::InvalidArgument);
    Ok(())
}

/// A real DNS write failure occurs after prefix insertion; it must roll back
/// both the new prefix and version, then allow the identical request to succeed.
#[crate::sqlx_test]
async fn add_prefix_dns_failure_rolls_back_and_retries(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::no_network_segments()).await;
    let seeds = HashMap::from([(
        "dns-failure".to_string(),
        underlay_seed("192.0.49.0/24", Some("192.0.49.1")),
    )]);
    crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
    let mut txn = pool.begin().await?;
    let target = db::network_segment::find_by_name(&mut txn, "dns-failure").await?;
    txn.commit().await?;
    let before = read_segment(&env.api, target.id).await;
    let request = forge::AddNetworkSegmentPrefixRequest {
        network_segment_id: Some(target.id),
        prefix: "2001:db8:49::/64".to_string(),
        ..Default::default()
    };
    let zone = db::dns::cidr_to_reverse_zone(request.prefix.parse()?).expect("aligned prefix");
    // The test-private database rejects this zone only, after the handler has
    // completed prefix/version writes. No production fault-injection seam.
    // `zone` is derived from the constant fixture CIDR, not caller-controlled SQL.
    let constraint = format!(
        "ALTER TABLE domains ADD CONSTRAINT test_added_prefix_dns_failure CHECK (name <> '{zone}')"
    );
    sqlx::query(sqlx::AssertSqlSafe(constraint.as_str()))
        .execute(&pool)
        .await?;
    let error = env
        .api
        .add_network_segment_prefix(Request::new(request.clone()))
        .await
        .expect_err("DNS failure");
    assert_eq!(error.code(), Code::Internal);
    assert!(
        std::error::Error::source(&error)
            .and_then(|source| source.source())
            .expect("database diagnostic below the redacted status wrapper")
            .to_string()
            .contains("test_added_prefix_dns_failure"),
        "{error}"
    );
    assert_eq!(read_segment(&env.api, target.id).await, before);
    sqlx::query("ALTER TABLE domains DROP CONSTRAINT test_added_prefix_dns_failure")
        .execute(&pool)
        .await?;
    let applied = env
        .api
        .add_network_segment_prefix(Request::new(request))
        .await?
        .into_inner();
    assert_eq!(
        config_version(&applied).version_nr(),
        target.version.increment().version_nr()
    );
    assert_eq!(read_segment(&env.api, target.id).await, applied);
    Ok(())
}

/// Competing public calls must re-read after the overlap guard: only one
/// different IPv6 request can win, with one version increment and reverse zone.
#[crate::sqlx_test]
async fn competing_prefix_additions_serialize(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::no_network_segments()).await;
    let seeds = HashMap::from([(
        "concurrent".to_string(),
        underlay_seed("192.0.50.0/24", Some("192.0.50.1")),
    )]);
    crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
    let mut blocker = pool.begin().await?;
    let target = db::network_segment::find_by_name(&mut blocker, "concurrent").await?;
    db::tenant_prefix_overlap::lock_checks(&mut blocker).await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let first = forge::AddNetworkSegmentPrefixRequest {
        network_segment_id: Some(target.id),
        prefix: "2001:db8:50::/64".to_string(),
        ..Default::default()
    };
    let second = forge::AddNetworkSegmentPrefixRequest {
        prefix: "2001:db8:51::/64".to_string(),
        ..first.clone()
    };
    let release = async {
        db::test_support::postgres::wait_for_blocked_queries(
            &pool,
            blocker_pid,
            "tenant_prefix_overlap:checks",
            2,
        )
        .await;
        blocker.commit().await
    };
    let (first, second, released) = tokio::join!(
        env.api.add_network_segment_prefix(Request::new(first)),
        env.api.add_network_segment_prefix(Request::new(second)),
        release,
    );
    released?;
    let (winner, loser) = match (first, second) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner.into_inner(), loser),
        result => panic!("exactly one request must win: {result:?}"),
    };
    assert_eq!(loser.code(), Code::FailedPrecondition);
    // A stale read followed by an optimistic-version failure has the same code.
    // This diagnostic proves that the loser read the winner's committed family.
    assert!(loser.message().contains("already has"), "{loser}");
    assert_eq!(
        config_version(&winner).version_nr(),
        target.version.increment().version_nr()
    );
    assert_eq!(winner.config.as_ref().expect("config").prefixes.len(), 2);
    assert_eq!(read_segment(&env.api, target.id).await, winner);
    let mut txn = pool.begin().await?;
    let mut zones = 0;
    for prefix in ["2001:db8:50::/64", "2001:db8:51::/64"] {
        let zone = db::dns::cidr_to_reverse_zone(prefix.parse()?).expect("aligned prefix");
        zones += db::dns::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone)
            .await?
            .len();
    }
    assert_eq!(zones, 1);
    txn.commit().await?;
    Ok(())
}

/// Deletion must wait before reading prefixes, then remove reverse zones for
/// an addition that was uncommitted when the delete request began.
#[crate::sqlx_test]
async fn deletion_reads_prefixes_after_pending_addition(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::no_network_segments()).await;
    let seeds = HashMap::from([(
        "delete-race".to_string(),
        underlay_seed("192.0.52.0/24", Some("192.0.52.1")),
    )]);
    crate::db_init::create_initial_networks(&env.api, &pool, &seeds).await?;
    let mut addition = pool.begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut addition).await?;
    let target = db::network_segment::find_by_name(&mut addition, "delete-race").await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *addition)
        .await?;
    let prefix = NewNetworkPrefix {
        prefix: "2001:db8:52::/64".parse()?,
        gateway: None,
        dhcpv6_link_address: None,
        num_reserved: 0,
    };
    // Hold the real append transaction at its post-insert/pre-commit boundary.
    db::network_segment::add_prefix(&target, &prefix, &mut addition).await?;
    db::dns::ensure_reverse_zones(&[prefix.prefix], &mut addition).await?;
    // Prove both zones exist before testing their removal, including the
    // uncommitted zone that deletion must wait to discover.
    for prefix in [target.prefixes[0].prefix, prefix.prefix] {
        let zone = db::dns::cidr_to_reverse_zone(prefix).expect("aligned prefix");
        assert_eq!(
            db::dns::domain::find_reverse_zone_by_normalized_name(addition.as_mut(), &zone)
                .await?
                .len(),
            1
        );
    }
    let release = async {
        db::test_support::postgres::wait_for_blocked_query(
            &pool,
            blocker_pid,
            "tenant_prefix_overlap:checks",
        )
        .await;
        addition.commit().await
    };
    let (deleted, released) = tokio::join!(
        env.api
            .delete_network_segment(Request::new(forge::NetworkSegmentDeletionRequest {
                id: Some(target.id)
            })),
        release
    );
    released?;
    deleted?;
    assert!(read_segment(&env.api, target.id).await.deleted.is_some());
    let mut txn = pool.begin().await?;
    for prefix in [target.prefixes[0].prefix, prefix.prefix] {
        let zone = db::dns::cidr_to_reverse_zone(prefix).expect("aligned prefix");
        assert!(
            db::dns::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &zone)
                .await?
                .is_empty()
        );
    }
    txn.commit().await?;
    Ok(())
}
