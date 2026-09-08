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

    // Query the ADM DNS record for this host — should now return both A and
    // AAAA records since the interface has both IPv4 and IPv6 addresses.
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

    // We should have at least 2 records: the original IPv4 (A) + our IPv6 (AAAA).
    assert!(
        dns_response.records.len() >= 2,
        "expected at least 2 records (A + AAAA), got {}",
        dns_response.records.len()
    );

    // Find the AAAA record and verify it.
    let aaaa_record = dns_response
        .records
        .iter()
        .find(|r| r.qtype == "AAAA")
        .expect("should have an AAAA record");
    assert_eq!(aaaa_record.content, "fd00::1");

    // Also verify the A record is still present and correct.
    let a_record = dns_response
        .records
        .iter()
        .find(|r| r.qtype == "A")
        .expect("should still have an A record");
    let a_ip: IpAddr = a_record.content.parse().unwrap();
    assert!(a_ip.is_ipv4(), "A record content should be an IPv4 address");

    // Also check the shortname view — the same interface's hostname should
    // produce both A and AAAA records via dns_records_shortname_combined.
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

    let shortname_aaaa = shortname_response
        .records
        .iter()
        .find(|r| r.qtype == "AAAA")
        .expect("shortname view should also have an AAAA record");
    assert_eq!(shortname_aaaa.content, "fd00::1");

    let shortname_a = shortname_response
        .records
        .iter()
        .find(|r| r.qtype == "A")
        .expect("shortname view should still have an A record");
    assert!(shortname_a.content.parse::<IpAddr>().unwrap().is_ipv4());
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
