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

    // Add an IPv6 address so the IPv6 reverse path has something to resolve.
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
// The PTR case runs inside the /24 reverse zone the admin segment creates. The
// fallback for an address with no enclosing reverse zone
// (`ptr_forward_authority`) needs a non-octet-aligned segment and is not
// covered here.
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
            description: "PTR inside the held reverse zone is Records",
            qname: ip_to_arpa(address),
            qtype: "PTR",
            outcome: DnsLookupOutcome::Records,
            authoritative: true,
            has_soa: false,
            record_count: 1,
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
