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

use std::net::IpAddr;

use carbide_api_core::test_support::network_segment::create_static_assignments_segment;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_uuid::machine::MachineId;
use const_format::concatcp;
use rpc::forge::DhcpDiscovery;
use tonic::Request;

const DOMAIN_NAME: &str = "dwrt1.com";
const DNS_ADM_SUBDOMAIN: &str = concatcp!("adm.", DOMAIN_NAME);
const DNS_BMC_SUBDOMAIN: &str = concatcp!("bmc.", DOMAIN_NAME);

struct DnsTestEnv {
    env: TestHarness,
    admin_segment: TestNetworkSegment,
    underlay_segment: TestNetworkSegment,
}

async fn init(pool: PgPool) -> DnsTestEnv {
    let resource_pools = ResourcePoolBuilder::default()
        .with_vlan_ids(1, 5)
        .with_vnis(10_001, 10_005)
        .build();
    let env = TestHarness::builder(pool)
        .with_resource_pools(resource_pools)
        .build()
        .await;
    let domain = env.create_test_domain(DOMAIN_NAME).await;
    let network_controller = env.network_controller();
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let vpc_id = network_controller.create_vpc("dns-test-vpc").await;
    network_controller
        .create_tenant_segment(&domain, vpc_id)
        .await;
    DnsTestEnv {
        env,
        admin_segment,
        underlay_segment,
    }
}

async fn create_managed_host(
    env: &TestHarness,
    underlay_segment: TestNetworkSegment,
    admin_segment: TestNetworkSegment,
) -> TestManagedHost {
    let site_explorer = env.default_test_site_explorer();
    env.managed_host_builder(&site_explorer, underlay_segment)
        .with_dpu_primary_interfaces(admin_segment)
        .with_dpu_network_status_reported()
        .build()
        .await
        .0
}

#[sqlx_test]
async fn test_domain_writes_reject_reverse_roots(pool: PgPool) {
    use rpc::protos::dns::{CreateDomainRequest, DomainSearchQuery, UpdateDomainRequest};

    let env = TestHarness::builder(pool).build().await;
    let api = env.api();
    let original = api
        .create_domain(Request::new(CreateDomainRequest {
            name: DOMAIN_NAME.to_string(),
            default_ttl: None,
            reserved_id: None,
        }))
        .await
        .expect("valid DNS test fixture")
        .into_inner();

    for name in [
        "in-addr.arpa",
        "ip6.arpa.",
        "IN-ADDR.ARPA.",
        "0.10.in-addr.arpa",
        " ip6.arpa. ",
    ] {
        let error = api
            .create_domain(Request::new(CreateDomainRequest {
                name: name.to_string(),
                default_ttl: None,
                reserved_id: None,
            }))
            .await
            .expect_err("reverse domain writes are rejected");
        assert_eq!(error.code(), tonic::Code::InvalidArgument, "create {name}");
        assert!(
            error.message().contains("reverse DNS zone"),
            "create {name}"
        );
    }

    // UpdateDomain cannot rename a domain at all, so the reverse-name rule
    // never applies there.
    let error = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                name: "renamed.example".to_string(),
                ..original.clone()
            }),
        }))
        .await
        .expect_err("renaming is rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "rename");
    assert!(error.message().contains("not supported"), "rename");

    let domains = api
        .find_domain(Request::new(DomainSearchQuery::default()))
        .await
        .expect("valid DNS test fixture")
        .into_inner()
        .domains;
    assert_eq!(
        domains,
        vec![original],
        "rejected writes must leave no changes"
    );
}

// UpdateDomain treats an empty or unchanged name and an absent default_ttl as
// "keep the stored value", and both Create and Update reject a TTL outside
// 30..=86400 with INVALID_ARGUMENT before anything is written.
#[sqlx_test]
async fn test_domain_default_ttl_omission_and_range_rules(pool: PgPool) {
    use rpc::protos::dns::{CreateDomainRequest, UpdateDomainRequest};

    let env = TestHarness::builder(pool).build().await;
    let api = env.api();
    let created = api
        .create_domain(Request::new(CreateDomainRequest {
            name: DOMAIN_NAME.to_string(),
            default_ttl: Some(600),
            reserved_id: None,
        }))
        .await
        .expect("create domain with a default TTL")
        .into_inner();
    assert_eq!(created.default_ttl, Some(600));

    // An update carrying only the id leaves both the name and the TTL as stored.
    let unchanged = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                id: created.id,
                ..Default::default()
            }),
        }))
        .await
        .expect("update without touching the name or TTL")
        .into_inner();
    assert_eq!(unchanged.name, created.name);
    assert_eq!(unchanged.default_ttl, Some(600));

    // Sending a fetched domain straight back, name included, is not a rename.
    let echoed = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(unchanged.clone()),
        }))
        .await
        .expect("echoing the stored name back")
        .into_inner();
    assert_eq!(echoed.name, created.name);

    let error = api
        .create_domain(Request::new(CreateDomainRequest {
            name: "short-ttl.example".to_string(),
            default_ttl: Some(5),
            reserved_id: None,
        }))
        .await
        .expect_err("TTL below the floor on create");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let error = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                id: created.id,
                default_ttl: Some(5),
                ..Default::default()
            }),
        }))
        .await
        .expect_err("TTL below the floor on update");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

/// Exercise the Core handler against disposable Postgres, not a mocked RPC.
/// The site-agent identity is set on the direct request because this fixture
/// bypasses the TLS/auth middleware; RBAC itself has separate principal tests.
#[sqlx_test]
async fn test_domain_reserved_id_replay_and_reference_guard(pool: PgPool) {
    use carbide_authn::middleware::Principal;
    use carbide_uuid::domain::DomainId;
    use rpc::protos::dns::{CreateDomainRequest, DomainDeletionRequest, UpdateDomainRequest};

    fn site_request<T>(payload: T) -> Request<T> {
        let mut request = Request::new(payload);
        request
            .extensions_mut()
            .insert(carbide_api_core::AuthContext {
                // Real mTLS authentication records both the identity minted
                // from the SPIFFE URI and the trusted-certificate marker.
                principals: vec![
                    Principal::SpiffeServiceIdentifier("elektra-site-agent".to_string()),
                    Principal::TrustedCertificate,
                ],
                authorization: None,
            });
        request
    }

    let env = TestHarness::builder(pool).build().await;
    let api = env.api();
    let id: DomainId = uuid::Uuid::new_v4().into();
    let payload = || CreateDomainRequest {
        name: "owned.example".to_string(),
        default_ttl: Some(600),
        reserved_id: Some(id),
    };
    // A site-agent service cannot drop the reserved-ID fence and obtain a
    // fresh operator-owned domain, even though RPC RBAC admits this method.
    let unreserved_site_create = api
        .create_domain(site_request(CreateDomainRequest {
            name: "unreserved-site.example".into(),
            default_ttl: None,
            reserved_id: None,
        }))
        .await
        .expect_err("SiteAgent must use the reserved create operation");
    assert_eq!(unreserved_site_create.code(), tonic::Code::PermissionDenied);

    let unauthorized = api
        .create_domain(Request::new(payload()))
        .await
        .expect_err("direct caller without SiteAgent identity cannot reserve an ID");
    assert_eq!(unauthorized.code(), tonic::Code::PermissionDenied);

    let mut mixed_identity_request = site_request(payload());
    mixed_identity_request
        .extensions_mut()
        .get_mut::<carbide_api_core::AuthContext>()
        .unwrap()
        .principals
        .push(Principal::SpiffeServiceIdentifier("nico-dns".to_string()));
    let mixed_identity = api
        .create_domain(mixed_identity_request)
        .await
        .expect_err("an additional service identity must not inherit SiteAgent authority");
    assert_eq!(mixed_identity.code(), tonic::Code::PermissionDenied);

    let mut reserved_operator_request = Request::new(payload());
    reserved_operator_request
        .extensions_mut()
        .insert(carbide_api_core::AuthContext {
            principals: vec![
                Principal::ExternalUser(carbide_authn::middleware::ExternalUserInfo::new(
                    None,
                    "nico-cli-client".into(),
                    None,
                )),
                Principal::TrustedCertificate,
            ],
            authorization: None,
        });
    let reserved_operator = api
        .create_domain(reserved_operator_request)
        .await
        .expect_err("an operator must not claim a reserved SiteAgent domain ID");
    assert_eq!(reserved_operator.code(), tonic::Code::PermissionDenied);

    let mut operator_request = Request::new(CreateDomainRequest {
        name: "operator-owned.example".into(),
        default_ttl: None,
        reserved_id: None,
    });
    operator_request
        .extensions_mut()
        .insert(carbide_api_core::AuthContext {
            principals: vec![
                Principal::ExternalUser(carbide_authn::middleware::ExternalUserInfo::new(
                    None,
                    "nico-cli-client".into(),
                    None,
                )),
                Principal::TrustedCertificate,
            ],
            authorization: None,
        });
    let operator_zone = api
        .create_domain(operator_request)
        .await
        .expect("operator create remains available")
        .into_inner();
    assert_ne!(operator_zone.id, Some(id));
    api.delete_domain(Request::new(DomainDeletionRequest {
        id: operator_zone.id,
        cancel_reserved_id: false,
    }))
    .await
    .expect("operator ordinary delete remains available");

    let created = api
        .create_domain(site_request(payload()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(created.id, Some(id));
    let first_created_at = created.created;
    let replay = api
        .create_domain(site_request(payload()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        replay.id,
        Some(id),
        "timeout replay cannot create another Core ID"
    );
    assert_eq!(
        replay.created, first_created_at,
        "replay preserves the original row"
    );
    let mut alias = payload();
    alias.name = "OWNED.Example.".to_string();
    assert_eq!(
        api.create_domain(site_request(alias))
            .await
            .expect("case/root-dot variations replay the same canonical intent")
            .into_inner()
            .id,
        Some(id)
    );

    let mut wrong_name = payload();
    wrong_name.name = "other.example".to_string();
    assert_eq!(
        api.create_domain(site_request(wrong_name))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );
    let mut wrong_ttl = payload();
    wrong_ttl.default_ttl = Some(601);
    assert_eq!(
        api.create_domain(site_request(wrong_ttl))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );

    // Mutating the live TTL cannot invalidate the immutable create snapshot.
    let updated = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                id: Some(id),
                default_ttl: Some(1800),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.default_ttl, Some(1800));
    let replay = api
        .create_domain(site_request(payload()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(replay.id, Some(id));
    assert_eq!(replay.default_ttl, Some(1800));

    // An attached segment is a live reference even when the Core domain
    // caller only knows the ID. The guard must leave both rows intact.
    let domain = env.create_test_domain("referenced.example").await;
    let segment = env.network_controller().create_admin_segment(&domain).await;
    let unauthorized_site_delete = api
        .delete_domain(site_request(DomainDeletionRequest {
            id: Some(domain.id),
            cancel_reserved_id: false,
        }))
        .await
        .expect_err("SiteAgent cannot perform an operator delete on another domain");
    assert_eq!(
        unauthorized_site_delete.code(),
        tonic::Code::PermissionDenied
    );
    let unauthorized_site_owned_delete = api
        .delete_domain(site_request(DomainDeletionRequest {
            id: Some(id),
            cancel_reserved_id: false,
        }))
        .await
        .expect_err("SiteAgent cannot omit the cancellation fence for its own domain");
    assert_eq!(
        unauthorized_site_owned_delete.code(),
        tonic::Code::PermissionDenied
    );
    let blocked = api
        .delete_domain(Request::new(DomainDeletionRequest {
            id: Some(domain.id),
            cancel_reserved_id: false,
        }))
        .await
        .expect_err("live subnet must prevent domain deletion");
    assert_eq!(blocked.code(), tonic::Code::FailedPrecondition);
    assert!(segment.id.to_string().len() > 0);

    api.delete_domain(Request::new(DomainDeletionRequest {
        id: Some(id),
        cancel_reserved_id: false,
    }))
    .await
    .unwrap();
    api.delete_domain(Request::new(DomainDeletionRequest {
        id: Some(id),
        cancel_reserved_id: false,
    }))
    .await
    .expect("repeat delete of a tombstoned domain is idempotent");
    let late_update = api
        .update_domain(Request::new(UpdateDomainRequest {
            domain: Some(rpc::protos::dns::Domain {
                id: Some(id),
                default_ttl: Some(900),
                ..Default::default()
            }),
        }))
        .await
        .expect_err("late TTL update cannot mutate a tombstone");
    assert_eq!(late_update.code(), tonic::Code::NotFound);
    assert_eq!(
        api.create_domain(site_request(payload()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::FailedPrecondition
    );

    let missing: DomainId = uuid::Uuid::new_v4().into();
    assert_eq!(
        api.delete_domain(Request::new(DomainDeletionRequest {
            id: Some(missing),
            cancel_reserved_id: false,
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::NotFound,
        "ordinary unknown domain deletion remains not-found"
    );
    assert_eq!(
        api.delete_domain(Request::new(DomainDeletionRequest {
            id: Some(missing),
            cancel_reserved_id: true,
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::PermissionDenied,
        "arbitrary callers cannot cancel a reserved ID"
    );
    let cancellation = || DomainDeletionRequest {
        id: Some(missing),
        cancel_reserved_id: true,
    };
    api.delete_domain(site_request(cancellation()))
        .await
        .unwrap();
    api.delete_domain(site_request(cancellation()))
        .await
        .expect("cancellation retries are idempotent");
    assert_eq!(
        api.create_domain(site_request(CreateDomainRequest {
            name: "late.example".to_string(),
            default_ttl: None,
            reserved_id: Some(missing),
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::FailedPrecondition,
        "a delayed create cannot resurrect an absent cancelled ID"
    );
    assert_eq!(
        api.delete_domain(site_request(DomainDeletionRequest {
            id: Some(domain.id),
            cancel_reserved_id: true,
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::FailedPrecondition,
        "SiteAgent cannot claim a legacy-created domain as reserved"
    );
}

#[sqlx_test]
async fn test_reserved_create_delete_concurrent_and_name_collision(pool: PgPool) {
    use carbide_authn::middleware::Principal;
    use carbide_uuid::domain::DomainId;
    use rpc::protos::dns::{CreateDomainRequest, DomainDeletionRequest};

    fn site_request<T>(payload: T) -> Request<T> {
        let mut request = Request::new(payload);
        request
            .extensions_mut()
            .insert(carbide_api_core::AuthContext {
                principals: vec![Principal::SpiffeServiceIdentifier(
                    "elektra-site-agent".into(),
                )],
                authorization: None,
            });
        request
    }

    let env = TestHarness::builder(pool).build().await;
    let api = env.api();
    let id: DomainId = uuid::Uuid::new_v4().into();
    let create = site_request(CreateDomainRequest {
        name: "race.example".into(),
        default_ttl: None,
        reserved_id: Some(id),
    });
    let cancel = site_request(DomainDeletionRequest {
        id: Some(id),
        cancel_reserved_id: true,
    });
    let (created, cancelled) = tokio::join!(api.create_domain(create), api.delete_domain(cancel));
    cancelled
        .expect("concurrent cancellation either tombstones absent ID or deletes the created row");
    if let Err(error) = created {
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    }
    assert_eq!(
        api.create_domain(site_request(CreateDomainRequest {
            name: "race.example".into(),
            default_ttl: None,
            reserved_id: Some(id),
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::FailedPrecondition,
        "a cancelled reserved ID must never become live"
    );

    // Repeated delivery of the *same* create intent must converge on one
    // stable row even when both calls begin before either has returned.
    // The ID advisory lock serializes the two transactions; this test does
    // not force them to overlap inside the database transaction.
    let replay_id: DomainId = uuid::Uuid::new_v4().into();
    let replay_payload = || {
        site_request(CreateDomainRequest {
            name: "same-intent.example".into(),
            default_ttl: Some(720),
            reserved_id: Some(replay_id),
        })
    };
    let (first, second) = tokio::join!(
        api.create_domain(replay_payload()),
        api.create_domain(replay_payload())
    );
    let first = first.expect("first reserved create succeeds").into_inner();
    let second = second.expect("same-ID replay succeeds").into_inner();
    assert_eq!(first.id, Some(replay_id));
    assert_eq!(second.id, first.id);
    assert_eq!(
        second.created, first.created,
        "replay returned the same row"
    );

    let a: DomainId = uuid::Uuid::new_v4().into();
    let b: DomainId = uuid::Uuid::new_v4().into();
    let payload = |id| {
        site_request(CreateDomainRequest {
            name: "same.example".into(),
            default_ttl: None,
            reserved_id: Some(id),
        })
    };
    let (one, two) = tokio::join!(api.create_domain(payload(a)), api.create_domain(payload(b)));
    assert_eq!(
        usize::from(one.is_ok()) + usize::from(two.is_ok()),
        1,
        "distinct reserved IDs cannot commit one normalized forward name twice"
    );
}

#[sqlx_test]
async fn test_dns(pool: PgPool) {
    let DnsTestEnv {
        env,
        admin_segment,
        underlay_segment,
    } = init(pool).await;
    let api = env.api();

    // Database should have 0 rows in the dns_records view.
    assert_eq!(
        0,
        db::test_support::dns::record_count(&api.database_connection).await
    );

    let mac_address = "FF:FF:FF:FF:FF:FF";
    let interface1 = api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let fqdn1 = interface1.fqdn;
    let ip1 = interface1.address;
    let mac_address = "F1:FF:FF:FF:FF:FF";
    let interface2 = api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let fqdn2 = interface2.fqdn;
    let ip2 = interface2.address;

    tracing::info!(fqdn1 = %fqdn1, "FQDN1");
    let dns_record = api
        .lookup_record(Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: fqdn1 + ".",
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
    tracing::info!(dns_record = ?dns_record, "DNS Record");
    tracing::info!(ip1 = %ip1, "IP");
    assert_eq!(
        ip1.split('/').collect::<Vec<&str>>()[0],
        &*dns_record.records[0].content
    );
    assert_eq!(
        dns_record.records[0].qtype, "A",
        "IPv4 record should have qtype A"
    );

    let dns_record = api
        .lookup_record(Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qtype: "A".to_string(),
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qname: fqdn2 + ".",
                real_remote: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        ip2.split('/').collect::<Vec<&str>>()[0],
        &*dns_record.records[0].content,
    );
    assert_eq!(
        dns_record.records[0].qtype, "A",
        "IPv4 record should have qtype A"
    );

    // Create a managed host to make sure that the MachineId DNS
    // records for the Host and DPU are created + end up in the
    // dns_records view.
    let managed_host = create_managed_host(&env, underlay_segment, admin_segment).await;

    // And now check to make sure the DNS records exist and,
    // of course, that they are correct.
    let machine_ids: [MachineId; 2] = [
        managed_host.host.id.into(),
        managed_host.first_dpu().id.into(),
    ];
    for machine_id in &machine_ids {
        let mut txn = env.db_txn().await;

        // First, check the BMC record by querying the MachineTopology
        // data for the current machine ID.
        tracing::info!(machine_id = %machine_id, subdomain = %DNS_BMC_SUBDOMAIN, "Checking BMC record");
        let topologies = db::machine_topology::find_by_machine_ids(&mut txn, &[*machine_id])
            .await
            .unwrap();
        let topology = &topologies.get(machine_id).unwrap()[0];
        let bmc_record = api
            .lookup_record(Request::new(
                rpc::protos::dns::DnsResourceRecordLookupRequest {
                    qname: format!("{}.{}.", machine_id, DNS_BMC_SUBDOMAIN),
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
        assert_eq!(
            topology.topology().bmc_info.ip.unwrap().to_string(),
            &*bmc_record.records[0].content
        );
        assert_eq!(
            bmc_record.records[0].qtype, "A",
            "BMC record should have qtype A"
        );

        // And now check the ADM (Admin IP) record by querying the
        // MachineInterface data for the given machineID.
        tracing::info!(machine_id = %machine_id, subdomain = %DNS_ADM_SUBDOMAIN, "Checking ADM record");
        let interface = db::machine_interface::get_machine_interface_primary(machine_id, &mut txn)
            .await
            .unwrap();
        let adm_record = api
            .lookup_record(Request::new(
                rpc::protos::dns::DnsResourceRecordLookupRequest {
                    qname: format!("{}.{}.", machine_id, DNS_ADM_SUBDOMAIN),
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
        assert_eq!(
            format!("{}", interface.addresses[0]).as_str(),
            &*adm_record.records[0].content
        );
        assert_eq!(
            adm_record.records[0].qtype, "A",
            "ADM record should have qtype A"
        );
        txn.rollback().await.unwrap();
    }

    // Database should ultimately have 10 rows:
    // - 4x from the DHCP discovery testing.
    // - 6x from the managed host testing.
    //      - 2x fancy names
    //      - 2x admin machine ID names
    //      - 2x bmc machine ID names
    assert_eq!(
        10,
        db::test_support::dns::record_count(&api.database_connection).await
    );

    let status = api
        .lookup_record(Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: "".to_string(),
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qtype: "A".to_string(),
                real_remote: None,
            },
        ))
        .await
        .expect_err("Query should return an error");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(status.message(), "qname cannot be empty");

    // Querying for something unknown should return an empty records Vec
    for name in [
        "unknown".to_string(),
        format!("unknown.{DNS_BMC_SUBDOMAIN}."),
    ] {
        let status = api
            .lookup_record(Request::new(
                rpc::protos::dns::DnsResourceRecordLookupRequest {
                    qname: name,
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

        tracing::info!(dns_lookup_response = ?status, "Status");
        assert_eq!(status.records.len(), 0);
    }
}

// test_dns_aaaa verifies that IPv6 addresses in the machine_interface_addresses
// table produce AAAA DNS records (not A records) in the dns_records view.
#[sqlx_test]
async fn test_dns_aaaa(pool: PgPool) {
    let DnsTestEnv {
        env,
        admin_segment,
        underlay_segment,
    } = init(pool).await;
    let api = env.api();
    let managed_host = create_managed_host(&env, underlay_segment, admin_segment).await;
    let host_id = managed_host.host.id;

    let mut txn = env.db_txn().await;

    // Get the primary interface for this host — it already has an IPv4 address
    // from the managed host creation flow.
    let interface = db::machine_interface::get_machine_interface_primary(&host_id, &mut txn)
        .await
        .unwrap();
    assert!(
        !interface.addresses.is_empty(),
        "interface should have at least one IPv4 address"
    );

    let ipv6_addr: IpAddr = "fd00::1".parse().unwrap();

    // Insert an IPv6 address directly for this interface. This simulates what
    // would happen in a dual-stack environment once DHCPv6 is implemented.
    sqlx::query("INSERT INTO machine_interface_addresses (interface_id, address) VALUES ($1, $2)")
        .bind(interface.id)
        .bind(ipv6_addr)
        .execute(&mut *txn)
        .await
        .unwrap();

    txn.commit().await.unwrap();

    // Query AAAA for the ADM name. The interface holds both an IPv4 and an IPv6
    // address, but the answer carries only records of the requested type.
    let adm_qname = format!("{}.{}.", host_id, DNS_ADM_SUBDOMAIN);
    let dns_response = api
        .lookup_record(Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: adm_qname,
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qtype: "AAAA".to_string(),
                real_remote: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(
        dns_response.records.iter().all(|r| r.qtype == "AAAA"),
        "an AAAA question returns only AAAA records, got {:?}",
        dns_response.records
    );
    let [aaaa_record] = dns_response.records.as_slice() else {
        panic!(
            "expected the one AAAA record, got {:?}",
            dns_response.records
        );
    };
    assert_eq!(aaaa_record.content, "fd00::1");

    // The same interface's hostname reaches the same addresses through
    // dns_records_shortname_combined.
    let shortname_qname = format!("{}.{}.", interface.hostname, DOMAIN_NAME);
    let shortname_response = api
        .lookup_record(Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: shortname_qname,
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qtype: "AAAA".to_string(),
                real_remote: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let [shortname_aaaa] = shortname_response.records.as_slice() else {
        panic!(
            "expected the one AAAA record from the shortname view, got {:?}",
            shortname_response.records
        );
    };
    assert_eq!(shortname_aaaa.qtype, "AAAA");
    assert_eq!(shortname_aaaa.content, "fd00::1");
}

// test_dns_ptr verifies that a reverse-DNS (PTR) query resolves an address to the
// fully-qualified hostname of the interface that holds it. The handler parses the
// in-addr.arpa / ip6.arpa qname back to an address and looks the interface up by
// address, so this exercises both the IPv4 and IPv6 reverse paths end to end.
#[sqlx_test]
async fn test_dns_ptr(pool: PgPool) {
    let DnsTestEnv {
        env,
        admin_segment,
        underlay_segment,
    } = init(pool).await;
    let api = env.api();
    let managed_host = create_managed_host(&env, underlay_segment, admin_segment).await;
    let host_id = managed_host.host.id;

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::get_machine_interface_primary(&host_id, &mut txn)
        .await
        .unwrap();

    // The primary interface already holds an IPv4 address from the managed host
    // creation flow; reuse it for the IPv4 reverse lookup. (An interface may hold
    // at most one address per family, so we cannot add a second IPv4 here.)
    let ipv4_addr = interface
        .addresses
        .iter()
        .copied()
        .find(|addr| addr.is_ipv4())
        .expect("primary interface should have an IPv4 address");

    // Insert the IPv6 prefix and address directly, bypassing the network-creation
    // code that maintains reverse-domain rows for rollback. The PTR must resolve
    // even though no corresponding reverse-domain row exists.
    let ipv6_prefix: ipnetwork::IpNetwork = "fd00::/64".parse().expect("valid DNS test fixture");
    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, num_reserved) VALUES ($1, $2, 0)",
    )
    .bind(interface.segment_id)
    .bind(ipv6_prefix)
    .execute(&mut *txn)
    .await
    .expect("valid DNS test fixture");
    let ipv6_addr: IpAddr = "fd00::1".parse().unwrap();
    sqlx::query("INSERT INTO machine_interface_addresses (interface_id, address) VALUES ($1, $2)")
        .bind(interface.id)
        .bind(ipv6_addr)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // PTR content is the interface's fully-qualified hostname, matching the
    // forward (shortname) view's name for the same interface.
    let expected_fqdn = format!("{}.{}.", interface.hostname, DOMAIN_NAME);

    // Each case issues one PTR lookup. `expected` is the FQDN of the single
    // record we expect back, or None when the query should resolve to nothing.
    struct PtrCase {
        description: &'static str,
        qname: String,
        expected: Option<String>,
    }

    let cases = [
        PtrCase {
            description: "IPv4 reverse lookup resolves to the interface FQDN",
            qname: ip_to_arpa(ipv4_addr),
            expected: Some(expected_fqdn.clone()),
        },
        PtrCase {
            description: "IPv6 reverse lookup resolves to the interface FQDN",
            qname: ip_to_arpa(ipv6_addr),
            expected: Some(expected_fqdn),
        },
        PtrCase {
            description: "an address no interface holds resolves to nothing",
            qname: "1.113.0.203.in-addr.arpa.".to_string(),
            expected: None,
        },
        PtrCase {
            description: "a qname that does not parse yields nothing, not an error",
            qname: "not.an.address.in-addr.arpa.".to_string(),
            expected: None,
        },
    ];

    for case in cases {
        let records = lookup_ptr(api, &case.qname).await;
        match case.expected {
            Some(expected_content) => {
                assert_eq!(
                    records.len(),
                    1,
                    "{}: expected one record",
                    case.description
                );
                assert_eq!(records[0].qtype, "PTR", "{}", case.description);
                assert_eq!(
                    records[0].qname, case.qname,
                    "{}: the queried qname is echoed back",
                    case.description
                );
                assert_eq!(records[0].content, expected_content, "{}", case.description);
            }
            None => assert!(records.is_empty(), "{}", case.description),
        }
    }
}

/// A configured static address outside managed prefixes still resolves to its
/// hostname through PTR lookup. That assignment does not authorize reverse SOA
/// answers or authoritative negative answers for neighbouring addresses or
/// parent reverse names.
#[sqlx_test]
async fn test_static_address_outside_managed_prefixes_ptr(pool: PgPool) {
    use rpc::protos::dns::DnsLookupOutcome;

    let env = TestHarness::builder(pool).build().await;
    let domain = env.create_test_domain(DOMAIN_NAME).await;
    let segment_id = create_static_assignments_segment(env.api(), Some(domain.id)).await;
    let address: IpAddr = "203.0.113.7".parse().expect("valid DNS test fixture");
    let mac_address = "02:00:00:00:de:07".parse().expect("valid DNS test fixture");
    let mut txn = env.db_txn().await;
    db::machine_interface::preallocate_machine_interface(&mut txn, mac_address, address, None)
        .await
        .expect("valid DNS test fixture");
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac_address)
        .await
        .expect("valid DNS test fixture");
    let [interface] = interfaces.as_slice() else {
        panic!("static preallocation must create one interface");
    };
    assert_eq!(interface.segment_id, segment_id);
    let fqdn = format!("{}.{}.", interface.hostname, domain.name);
    txn.commit().await.expect("valid DNS test fixture");

    let qname = ip_to_arpa(address);
    let cases = [
        (
            "forward name remains published",
            fqdn.clone(),
            "A",
            Some(address.to_string()),
        ),
        (
            "static address outside managed prefixes has a PTR",
            qname.clone(),
            "PTR",
            Some(fqdn),
        ),
        ("no synthetic host zone", qname, "SOA", None),
        (
            "unowned neighbour",
            "8.113.0.203.in-addr.arpa.".to_string(),
            "PTR",
            None,
        ),
        (
            "unheld parent",
            "113.0.203.in-addr.arpa.".to_string(),
            "PTR",
            None,
        ),
    ];
    for (description, qname, qtype, content) in cases {
        let response = env
            .api()
            .lookup_record(Request::new(
                rpc::protos::dns::DnsResourceRecordLookupRequest {
                    qname: qname.clone(),
                    qtype: qtype.to_string(),
                    zone_id: "-1".to_string(),
                    local: None,
                    remote: None,
                    real_remote: None,
                },
            ))
            .await
            .expect("valid DNS test fixture")
            .into_inner();
        let outcome = if content.is_some() {
            DnsLookupOutcome::Records
        } else {
            DnsLookupOutcome::NotAuthoritative
        };
        assert_eq!(response.outcome, outcome as i32, "{description}");
        assert_eq!(response.authoritative, content.is_some(), "{description}");
        assert!(response.authority_soa.is_none(), "{description}");
        match content {
            Some(content) => {
                let [record] = response.records.as_slice() else {
                    panic!(
                        "{description}: expected one record, got {:?}",
                        response.records
                    );
                };
                assert_eq!(record.qname, qname, "{description}");
                assert_eq!(record.qtype, qtype, "{description}");
                assert_eq!(record.content, content, "{description}");
            }
            None => assert!(response.records.is_empty(), "{description}"),
        }
    }
}

/// Issue a PTR `lookup_record` query and return the reply records.
async fn lookup_ptr(api: &Api, qname: &str) -> Vec<rpc::protos::dns::DnsResourceRecord> {
    api.lookup_record(Request::new(
        rpc::protos::dns::DnsResourceRecordLookupRequest {
            qname: qname.to_string(),
            zone_id: uuid::Uuid::new_v4().to_string(),
            local: None,
            remote: None,
            qtype: "PTR".to_string(),
            real_remote: None,
        },
    ))
    .await
    .unwrap()
    .into_inner()
    .records
}

// test_dns_lookup_outcomes checks the `outcome`, `authoritative`, and
// `authority_soa` fields on a lookup response, not just the records.
//
// One DHCP discovery publishes a single A record under `dwrt1.com`. Each case
// then queries a name and expects one of:
//
// - Records: the requested type exists at the name.
// - NoData: the name exists (records of another type, or the zone apex) but
//   not the requested type. Carries the zone SOA.
// - NoSuchName: nothing at the name and nothing below it. Carries the zone SOA.
// - NotAuthoritative: no zone we hold contains the name. No SOA, AA clear.
//
// A managed host adds `<machine-id>.adm.dwrt1.com`, which makes
// `adm.dwrt1.com` an empty non-terminal: a name with nothing published at it
// but records below it (RFC 8020 §2). It must be NoData, not NoSuchName.
//
// Three cases exist to guard specific regressions: SOA at a non-apex name must
// be NoData rather than the A records at that name, `_dmarc.dwrt1.com` must
// classify like any other name rather than fail to parse, and a PTR question
// for a forward name that has an A record must be NoData rather than
// NoSuchName (a cached NXDOMAIN there would suppress the A lookup too).
//
// Owning an address permits a positive PTR answer, not authority over its
// enclosing reverse zone. Other record types and reverse names that do not
// encode a complete IP address must return NotAuthoritative.
#[sqlx_test]
async fn test_dns_lookup_outcomes(pool: PgPool) {
    use rpc::protos::dns::DnsLookupOutcome;

    let DnsTestEnv {
        env,
        admin_segment,
        underlay_segment,
    } = init(pool).await;
    let api = env.api();

    create_managed_host(&env, underlay_segment, admin_segment).await;

    let interface = api
        .discover_dhcp(
            DhcpDiscovery::builder("FF:FF:FF:FF:FF:FF", admin_segment.relay_address)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();
    let fqdn = format!("{}.", interface.fqdn);
    let address: IpAddr = interface
        .address
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    struct OutcomeCase {
        description: &'static str,
        qname: String,
        qtype: &'static str,
        outcome: DnsLookupOutcome,
        authoritative: bool,
        has_soa: bool,
        record_count: usize,
    }

    // Network creation maintains this address's reverse-domain row for rollback.
    // Add the ARPA roots to simulate legacy stored domains. None of these rows
    // may grant reverse-zone authority to the inventory-based lookup path.
    let reverse_zone = ip_to_arpa(address)
        .split_once('.')
        .expect("a reverse address contains labels")
        .1
        .to_string();
    let mut txn = env.db_txn().await;
    assert_eq!(
        db::dns::domain::find_reverse_zone_by_normalized_name(txn.as_mut(), &reverse_zone)
            .await
            .expect("maintained reverse zone lookup succeeds")
            .len(),
        1,
        "network creation maintains the rollback zone",
    );
    for name in ["in-addr.arpa", "ip6.arpa"] {
        db::dns::domain::persist(model::dns::NewDomain::new(name), &mut txn)
            .await
            .expect("legacy reverse domain fixture persists");
    }
    txn.commit().await.expect("legacy reverse fixtures commit");

    let cases = [
        OutcomeCase {
            description: "published A record is Records",
            qname: fqdn.clone(),
            qtype: "A",
            outcome: DnsLookupOutcome::Records,
            authoritative: true,
            has_soa: false,
            record_count: 1,
        },
        OutcomeCase {
            description: "name exists but has no AAAA is NoData with SOA",
            qname: fqdn.clone(),
            qtype: "AAAA",
            outcome: DnsLookupOutcome::NoData,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "SOA at a non-apex name is NoData, not the zone SOA",
            qname: fqdn.clone(),
            qtype: "SOA",
            outcome: DnsLookupOutcome::NoData,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "PTR at a forward name that exists is NoData, not NoSuchName",
            qname: fqdn.clone(),
            qtype: "PTR",
            outcome: DnsLookupOutcome::NoData,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "empty non-terminal with records below it is NoData",
            qname: format!("{DNS_ADM_SUBDOMAIN}."),
            qtype: "A",
            outcome: DnsLookupOutcome::NoData,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "apex SOA is the zone SOA",
            qname: format!("{DOMAIN_NAME}."),
            qtype: "SOA",
            outcome: DnsLookupOutcome::Records,
            authoritative: true,
            has_soa: false,
            record_count: 1,
        },
        OutcomeCase {
            description: "apex NS is NoData because NS is not published",
            qname: format!("{DOMAIN_NAME}."),
            qtype: "NS",
            outcome: DnsLookupOutcome::NoData,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "missing in-zone name is NoSuchName with SOA",
            qname: format!("no-such-host.{DOMAIN_NAME}."),
            qtype: "A",
            outcome: DnsLookupOutcome::NoSuchName,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "non-hostname label classifies instead of failing to parse",
            qname: format!("_dmarc.{DOMAIN_NAME}."),
            qtype: "TXT",
            outcome: DnsLookupOutcome::NoSuchName,
            authoritative: true,
            has_soa: true,
            record_count: 0,
        },
        OutcomeCase {
            description: "name outside every held zone is NotAuthoritative",
            qname: "www.example.org.".to_string(),
            qtype: "A",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
        OutcomeCase {
            description: "a published PTR remains available with retained reverse rows",
            qname: ip_to_arpa(address),
            qtype: "PTR",
            outcome: DnsLookupOutcome::Records,
            authoritative: true,
            has_soa: false,
            record_count: 1,
        },
        OutcomeCase {
            description: "an existing PTR name has no A record",
            qname: ip_to_arpa(address),
            qtype: "A",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
        OutcomeCase {
            description: "non-address reverse labels do not grant authority",
            qname: format!("invalid.{}", ip_to_arpa(address)),
            qtype: "PTR",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
        OutcomeCase {
            description: "retained reverse zone does not supply an apex SOA",
            qname: reverse_zone,
            qtype: "SOA",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
        OutcomeCase {
            description: "retained IPv4 root does not grant authority",
            qname: "in-addr.arpa.".to_string(),
            qtype: "SOA",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
        OutcomeCase {
            description: "retained IPv6 root does not deny missing addresses",
            qname: ip_to_arpa("2001:db8::dead".parse().expect("valid IPv6 fixture")),
            qtype: "PTR",
            outcome: DnsLookupOutcome::NotAuthoritative,
            authoritative: false,
            has_soa: false,
            record_count: 0,
        },
    ];

    for case in cases {
        let response = api
            .lookup_record(Request::new(
                rpc::protos::dns::DnsResourceRecordLookupRequest {
                    qname: case.qname.clone(),
                    zone_id: uuid::Uuid::new_v4().to_string(),
                    local: None,
                    remote: None,
                    qtype: case.qtype.to_string(),
                    real_remote: None,
                },
            ))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            response.outcome, case.outcome as i32,
            "{}: outcome",
            case.description
        );
        assert_eq!(
            response.authoritative, case.authoritative,
            "{}: authoritative",
            case.description
        );
        assert_eq!(
            response.authority_soa.is_some(),
            case.has_soa,
            "{}: authority SOA",
            case.description
        );
        assert_eq!(
            response.records.len(),
            case.record_count,
            "{}: record count",
            case.description
        );
        for record in &response.records {
            assert_eq!(
                record.qtype, case.qtype,
                "{}: record type",
                case.description
            );
        }
    }
}

/// Build the reverse-DNS qname for an address: the octets (IPv4) or nibbles
/// (IPv6) in reverse order, each as its own label, then the arpa suffix.
fn ip_to_arpa(addr: IpAddr) -> String {
    let mut qname = String::new();
    match addr {
        IpAddr::V4(addr) => {
            for octet in addr.octets().into_iter().rev() {
                qname.push_str(&format!("{octet}."));
            }
            qname.push_str("in-addr.arpa.");
        }
        IpAddr::V6(addr) => {
            for octet in addr.octets().into_iter().rev() {
                qname.push_str(&format!("{:x}.{:x}.", octet & 0x0f, octet >> 4));
            }
            qname.push_str("ip6.arpa.");
        }
    }
    qname
}
