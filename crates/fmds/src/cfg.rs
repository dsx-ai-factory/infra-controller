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

use std::net::SocketAddr;

use clap::Parser;
use ipnetwork::IpNetwork;

#[derive(Parser)]
#[clap(name = "carbide-fmds")]
pub struct Options {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    /// gRPC listen address for receiving config updates from
    /// carbide-dpu-agent. The tenant should not be able to
    /// communicate with this address.
    #[clap(long, default_value = "0.0.0.0:50052")]
    pub grpc_address: SocketAddr,

    /// REST listen address for tenant OS metadata queries.
    #[clap(long, default_value = "0.0.0.0:80")]
    pub rest_address: SocketAddr,

    /// Optional additional REST listen address for compatibility. When
    /// omitted, FMDS listens only on `rest_address`.
    #[clap(long)]
    pub compatibility_rest_address: Option<SocketAddr>,

    /// Prometheus scrape address for `/metrics` (HTTP request stats for the REST metadata API).
    #[clap(long, default_value = "0.0.0.0:8888")]
    pub metrics_address: SocketAddr,

    /// Carbide API server address for phone_home.
    #[clap(long, default_value = "https://carbide-api.forge")]
    pub forge_api: String,

    /// Path to root CA certificate.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub root_ca: Option<String>,

    /// Path to client certificate.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub client_cert: Option<String>,

    /// Path to client key.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub client_key: Option<String>,

    /// Path of the dpu-agent's local API socket. When set, fmds fetches
    /// short-lived node-auth bearer JWTs from the agent and presents them to
    /// carbide-api — instead of (or in addition to) the mTLS client cert —
    /// so this pod no longer needs the machine private key mounted.
    #[clap(long, env = "FMDS_NODE_TOKEN_SOCKET")]
    pub node_token_socket: Option<String>,

    /// Name of the interface to assign the metadata-service address to.
    #[clap(long, env = "FMDS_INTERFACE_NAME", default_value = "f_pf0hpf_if")]
    pub interface_name: String,

    /// CIDR to assign on `interface_name` (cloud metadata-service address).
    #[clap(
        long,
        env = "FMDS_INTERFACE_CIDR",
        default_value = "169.254.169.254/30"
    )]
    pub interface_cidr: IpNetwork,
}

impl Options {
    pub fn load() -> Self {
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_rest_address_is_disabled_by_default() {
        let options = Options::try_parse_from(["carbide-fmds"]).unwrap();

        assert_eq!(options.rest_address, "0.0.0.0:80".parse().unwrap());
        assert_eq!(options.compatibility_rest_address, None);
    }

    #[test]
    fn rest_addresses_can_be_overridden_independently() {
        let options = Options::try_parse_from([
            "carbide-fmds",
            "--rest-address",
            "127.0.0.1:8080",
            "--compatibility-rest-address",
            "127.0.0.1:7778",
        ])
        .unwrap();

        assert_eq!(options.rest_address, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(
            options.compatibility_rest_address,
            Some("127.0.0.1:7778".parse().unwrap())
        );
    }
}
