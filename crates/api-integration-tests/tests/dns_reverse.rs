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

//! Reverse-DNS lookup through the running API server's gRPC endpoint.
//!
//! Exercises startup network seeding, DHCP publication, and lookup wiring.
//! DNS packet encoding and the resolver's RCODE mapping are tested separately.

use std::net::{Ipv4Addr, SocketAddr};

use api_test_helper::api_server::TEST_BMC_DHCP_RELAY_ADDRESS;
use api_test_helper::utils::TestApiServerArgs;
use api_test_helper::{IntegrationTestEnvironment, dns, utils};
use eyre::Context;
use rpc::dns::DnsLookupOutcome;
use tokio_util::sync::CancellationToken;

/// An address in the BMC network that discovery will not hand out. Allocation
/// starts at the bottom of the `/23`, and only one address is discovered below.
const UNOWNED_BMC_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 1, 200);

/// A locally administered MAC in this test's isolated database.
const DISCOVERY_MAC_ADDRESS: &str = "02:00:00:00:de:ad";

#[ctor::ctor(unsafe)]
fn setup() {
    api_test_helper::setup_logging()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_reverse_dns_integration() -> eyre::Result<()> {
    let Some(mut test_env) =
        IntegrationTestEnvironment::try_from_environment(1, "api_server_test_dns_reverse")
            .await
            .context("initialize the reverse DNS test environment")?
    else {
        return Ok(());
    };

    // DHCP publication does not require BMC or firmware fixtures.
    let empty_firmware_dir = temp_dir::TempDir::with_prefix("firmware")
        .context("create the empty firmware directory")?;
    let cancel_token = CancellationToken::new();
    let server_handle = utils::start_api_server(
        &mut test_env,
        TestApiServerArgs {
            bmc_proxy: None,
            firmware_directory: empty_firmware_dir.path().to_owned(),
            addr_index: 0,
            put_dev_bin_in_path: false,
            insecure_discovery: true,
        },
        cancel_token.clone(),
    )
    .await
    .context("start the API server")?;

    // Join the server even when a lookup fails, before propagating that error.
    let result = run_reverse_dns_test(&test_env.carbide_api_addrs).await;

    cancel_token.cancel();
    let shutdown = server_handle.wait().await;
    test_env.db_pool.close().await;
    result.context("verify reverse DNS responses")?;
    shutdown.context("stop the API server")?;
    Ok(())
}

async fn run_reverse_dns_test(carbide_api_addrs: &[SocketAddr]) -> eyre::Result<()> {
    let discovered = dns::discover_dhcp(
        carbide_api_addrs,
        DISCOVERY_MAC_ADDRESS,
        TEST_BMC_DHCP_RELAY_ADDRESS,
    )
    .await
    .context("publish the DHCP fixture")?;
    let discovered_address: Ipv4Addr = discovered
        .address
        .split('/')
        .next()
        .expect("split always yields a first element")
        .parse()
        .context("parse the discovered IPv4 address")?;
    eyre::ensure!(
        discovered_address != UNOWNED_BMC_ADDRESS,
        "DHCP discovery allocated {UNOWNED_BMC_ADDRESS}, which this test needs to be unowned"
    );

    let fqdn = format!("{}.", discovered.fqdn);
    let cases = [
        (
            "allocated address",
            arpa_qname(discovered_address),
            DnsLookupOutcome::Records,
            Some(fqdn),
        ),
        (
            "unowned address in the managed prefix",
            arpa_qname(UNOWNED_BMC_ADDRESS),
            DnsLookupOutcome::NotAuthoritative,
            None,
        ),
        (
            "address outside the /23 but inside its enclosing /16",
            arpa_qname(Ipv4Addr::new(127, 0, 2, 1)),
            DnsLookupOutcome::NotAuthoritative,
            None,
        ),
    ];

    for (description, qname, outcome, content) in cases {
        let response = dns::lookup(carbide_api_addrs, &qname, "PTR")
            .await
            .context("look up the reverse DNS fixture")?;
        assert_eq!(response.outcome, outcome as i32, "{description}: outcome");
        assert_eq!(
            response.authoritative,
            outcome != DnsLookupOutcome::NotAuthoritative,
            "{description}: AA bit"
        );

        if let Some(content) = content {
            let [record] = response.records.as_slice() else {
                panic!(
                    "{description}: expected one PTR record, got {:?}",
                    response.records
                );
            };
            assert_eq!(record.qtype, "PTR", "{description}: record type");
            assert_eq!(record.qname, qname, "{description}: record name");
            assert_eq!(record.content, content, "{description}: content");
        } else {
            assert!(response.records.is_empty(), "{description}: no records");
        }

        assert!(response.authority_soa.is_none(), "{description}: no SOA");
    }

    Ok(())
}

/// The reverse qname for an IPv4 address: the octets reversed, one per label.
fn arpa_qname(address: Ipv4Addr) -> String {
    let [a, b, c, d] = address.octets();
    format!("{d}.{c}.{b}.{a}.in-addr.arpa.")
}
