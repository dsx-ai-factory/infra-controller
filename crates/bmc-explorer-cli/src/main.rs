/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use carbide_secrets::credentials::Credentials;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_site_explorer::config::SiteExplorerExploreMode;
use carbide_site_explorer::{AuthenticatedBmcClient, BmcEndpointExplorer};
use clap::Parser;
use mac_address::MacAddress;
use tracing_subscriber::fmt;

#[derive(Debug, Parser)]
#[command(
    name = "bmc-explorer-cli",
    about = "Explore BMC endpoints and generate reports."
)]
struct Cli {
    /// Username for BMC authentication
    #[arg(long)]
    username: String,

    /// Password for BMC authentication
    #[arg(long)]
    password: String,

    /// Exploration mode: one of `libredfish`, `nv-redfish`, or `compare-result`
    ///
    /// Defaults to `compare-result`.
    #[arg(long, default_value = "compare-result")]
    mode: String,

    /// Run benchmark instead of printing result; value is number of iterations
    #[arg(long)]
    benchmark: Option<u64>,

    /// IP address of the BMC (e.g. 192.168.0.10)
    ///
    /// First positional argument.
    bmc_ip: String,

    /// Port of the BMC (e.g. 443)
    #[arg(long, default_value_t = 443)]
    bmc_port: u16,

    /// Boot interface MAC address (e.g. 02:03:04:05:06:07)
    #[arg(long)]
    boot_mac: Option<MacAddress>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing so logs go to stderr.
    fmt().with_writer(std::io::stderr).init();

    let args = Cli::parse();

    let fallback_credentials = Credentials::UsernamePassword {
        username: args.username,
        password: args.password,
    };

    let rf_pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .build()?;
    let proxy_address = Arc::new(ArcSwap::new(None.into()));
    let credential_provider = Arc::new(TestCredentialManager::new(fallback_credentials.clone()));

    // The explorer performs credential-lifecycle work, so it takes the
    // credential-operations handle of the direct pool.
    let (_, redfish_client_pool) = carbide_redfish::libredfish::new_pool_with_credential_ops(
        credential_provider.clone(),
        rf_pool,
        proxy_address.clone(),
    );
    let mode = match args.mode.as_str() {
        "libredfish" => SiteExplorerExploreMode::LibRedfish,
        "nv-redfish" => SiteExplorerExploreMode::NvRedfish,
        "compare-result" => SiteExplorerExploreMode::CompareResult,
        other => {
            eprintln!(
                "Invalid mode '{other}'. Valid values are: libredfish, nv-redfish, compare-result."
            );
            std::process::exit(1);
        }
    };
    let rotate_switch_nvos_credentials = Default::default();

    let bmc_client = Arc::new(AuthenticatedBmcClient::new(
        redfish_client_pool,
        Arc::new(NvRedfishClientPool::new(proxy_address)),
        carbide_ipmi::test_support(),
        credential_provider.clone(),
    ));
    let explorer = BmcEndpointExplorer::new(
        bmc_client,
        rotate_switch_nvos_credentials,
        mode,
        // Standalone debug tool: no database, so rotation bookkeeping is skipped.
        None,
    );

    let ip = args.bmc_ip.parse()?;
    let port = args.bmc_port;
    let bmc_ip_address = SocketAddr::new(ip, port);
    let boot_interface = args.boot_mac.map(BootInterfaceTarget::MacOnly);

    if let Some(iterations) = args.benchmark {
        let start = Instant::now();
        for _ in 0..iterations {
            explorer
                .generate_exploration_report(
                    bmc_ip_address,
                    fallback_credentials.clone(),
                    boot_interface.as_ref(),
                    None,
                )
                .await?;
        }
        let elapsed = start.elapsed();
        println!("Benchmark: ran {iterations} iterations in {:.3?}", elapsed);
    } else {
        println!(
            "{}",
            serde_json::to_string(
                &explorer
                    .generate_exploration_report(
                        bmc_ip_address,
                        fallback_credentials.clone(),
                        boot_interface.as_ref(),
                        None,
                    )
                    .await?,
            )
            .expect("serialization success")
        );
    }

    Ok(())
}
